#!/usr/bin/env python3
"""Standard-library client for VibeStudio's asynchronous comparison API."""
import argparse
import json
import os
from pathlib import Path
import sys
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request


def request_api(base, path, payload=None):
    url = base.rstrip("/") + "/api/" + path
    data = None if payload is None else json.dumps(payload).encode()
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        try:
            message = json.load(error).get("error", str(error))
        except (ValueError, AttributeError):
            message = str(error)
        raise RuntimeError(message) from error


def request(base, operation, payload=None):
    return request_api(base, "comparison/" + operation, payload)


def tmux_identity():
    """Read only the invoking pane; never choose a session by working directory."""
    pane = os.environ.get("TMUX_PANE")
    if not pane or not os.environ.get("TMUX"):
        return {}
    try:
        output = subprocess.run(
            ["tmux", "display-message", "-p", "-t", pane,
             "#{session_name}\t#{@ass_agent}\t#{@ass_session_id}"],
            check=True, capture_output=True, text=True, timeout=3,
        ).stdout.rstrip("\n").split("\t")
    except (OSError, subprocess.SubprocessError):
        return {}
    if not output[0].startswith("ass-"):
        return {}
    owner = {"terminalId": output[0]}
    if len(output) > 1 and output[1]:
        owner["provider"] = output[1]
    if len(output) > 2 and output[2]:
        owner["conversationId"] = output[2]
    return owner


def associate(base, config, args):
    """Attach exact session provenance or require the caller to supply it."""
    artifact = dict(config.get("artifact") or {})
    owner = dict(artifact.get("owner") or {})
    if not owner:
        owner = tmux_identity()
    if args.session:
        # An explicit terminal must not inherit the invoking pane's conversation.
        if owner.get("terminalId") != args.session:
            owner = {"hostId": owner["hostId"]} if owner.get("hostId") else {}
        owner["terminalId"] = args.session
    if args.host:
        owner["hostId"] = args.host
    if not owner.get("hostId"):
        if os.environ.get("SSH_CONNECTION") or os.environ.get("SSH_CLIENT"):
            raise RuntimeError("For an SSH agent, supply --host with VibeStudio's exact workspace identifier, or set artifact.owner.hostId.")
        owner["hostId"] = "local"
    if not owner.get("terminalId") and not (owner.get("provider") and owner.get("conversationId")):
        raise RuntimeError("Associate this UI diff with a session: supply --session <terminal-id>, run inside its VibeStudio terminal, or set artifact.owner with an exact conversation identity.")
    # Only enrich from the selected workspace when it is the declared owner.
    # A local comparison listener may currently be showing a different SSH host.
    try:
        status = request_api(base, "remote/status")
        selected_host = status.get("host") or "local"
        if selected_host == owner["hostId"] and status.get("state") in ("idle", "local", "connected"):
            sessions = request_api(base, "terminal/list")
            matches = [item for item in sessions if item.get("id") == owner.get("terminalId")]
            if len(matches) == 1:
                session = matches[0]
                provider = session.get("agent")
                if provider and owner.get("provider", provider) == provider:
                    owner.setdefault("provider", provider)
                    if session.get("sessionId"):
                        owner.setdefault("conversationId", session["sessionId"])
    except (OSError, ValueError, RuntimeError, urllib.error.URLError):
        # Explicit/tmux provenance still identifies the terminal during outages.
        pass
    artifact["owner"] = owner
    artifact["title"] = args.title or artifact.get("title") or f"{Path(config.get('repository') or 'Project').name or 'Project'} UI diff"
    if args.description is not None:
        artifact["description"] = args.description
    config["artifact"] = artifact
    return config


def discover(explicit):
    base = explicit or os.environ.get("VIBESTUDIO_COMPARISON_URL")
    if not base:
        config = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
        record = config / "vibestudio" / "comparison-desktop.json"
        try:
            base = json.loads(record.read_text())["baseUrl"]
        except (OSError, ValueError, KeyError) as error:
            raise RuntimeError("Open VibeStudio desktop, or supply --server with its comparison listener URL.") from error
    parsed = urllib.parse.urlparse(base)
    if parsed.scheme not in ("http", "https") or not parsed.hostname:
        raise RuntimeError("The comparison listener must be an HTTP(S) URL.")
    capabilities = request(base, "capabilities")
    if capabilities.get("protocol") != 1 or not capabilities.get("available"):
        raise RuntimeError("This listener does not support live UI comparison protocol 1.")
    if not capabilities.get("sessionArtifacts"):
        raise RuntimeError("Restart the updated VibeStudio desktop to use session-associated UI diffs.")
    return base


def read_config(path):
    value = json.load(sys.stdin) if path == "-" else json.loads(Path(path).read_text())
    if not isinstance(value, dict):
        raise RuntimeError("Configuration must be a JSON object.")
    return value


def wait_for(base, session, wanted, timeout):
    deadline = time.monotonic() + timeout
    while session["state"] != wanted or (wanted == "ready" and not session.get("windowOpen", False)):
        if session["state"] in ("failed", "stopped"):
            raise RuntimeError(f"Session {session['id']}: {session.get('error') or session['state']}")
        if time.monotonic() >= deadline:
            raise RuntimeError(f"Timed out waiting for {session['id']}; inspect status or stop it explicitly.")
        time.sleep(0.25)
        session = request(base, "status?" + urllib.parse.urlencode({"id": session["id"]}))
    return session


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", help="Desktop switchboard URL; otherwise discover locally")
    commands = parser.add_subparsers(dest="operation", required=True)
    commands.add_parser("list")
    for name in ("start", "update", "status", "stop", "open", "close"):
        command = commands.add_parser(name)
        if name != "start":
            command.add_argument("id")
        if name in ("start", "update", "open"):
            command.add_argument("--config", required=name != "open", help="JSON file, or - for stdin; optional restore configuration for open")
        if name == "start":
            command.add_argument("--session", help="Owning VibeStudio terminal ID; defaults to the invoking tmux pane")
            command.add_argument("--host", help="Owning workspace identifier: local, or exact SSH/WSL target")
            command.add_argument("--title", help="Name shown in the session's UI diffs")
            command.add_argument("--description", help="What changed and what the user should review")
        if name in ("start", "stop", "open", "close"):
            command.add_argument("--wait", action="store_true")
            command.add_argument("--timeout", type=float, default=180)
    args = parser.parse_args()
    try:
        base = discover(args.server)
        if args.operation == "list":
            result = request(base, "list")
        elif args.operation == "status":
            result = request(base, "status?" + urllib.parse.urlencode({"id": args.id}))
        else:
            body = read_config(args.config) if args.operation in ("start", "update") else {}
            if args.operation == "start":
                body = associate(base, body, args)
            if args.operation == "open" and args.config:
                body["config"] = read_config(args.config)
            if args.operation != "start":
                body["id"] = args.id
            result = request(base, args.operation, body)
            if getattr(args, "wait", False):
                if args.operation == "close":
                    deadline = time.monotonic() + args.timeout
                    while result.get("windowOpen"):
                        if time.monotonic() >= deadline:
                            raise RuntimeError(f"Timed out closing {args.id}; inspect status.")
                        time.sleep(0.25)
                        result = request(base, "status?" + urllib.parse.urlencode({"id": args.id}))
                else:
                    result = wait_for(base, result, "ready" if args.operation in ("start", "open") else "stopped", args.timeout)
        print(json.dumps(result, indent=2))
        return 0
    except (OSError, ValueError, RuntimeError, urllib.error.URLError) as error:
        print(f"ui-compare: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
