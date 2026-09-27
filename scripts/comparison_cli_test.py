"""Exact session provenance must survive host changes and explicit overrides."""
import argparse
import importlib.util
import os
import sys
from pathlib import Path
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location("compare", Path(__file__).resolve().parents[1] / "skills/ui-compare/scripts/compare.py")
compare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compare)


def args(**values):
    return argparse.Namespace(**dict({"session": None, "host": None, "title": None, "description": None}, **values))


class AssociationTests(unittest.TestCase):
    @patch.dict(os.environ, {}, clear=True)
    def test_rejects_missing_identity_instead_of_matching_repository(self):
        with patch.object(compare, "tmux_identity", return_value={}):
            with self.assertRaisesRegex(RuntimeError, "Associate this UI diff"):
                compare.associate("http://fixture", {"repository": "/shared/repo"}, args())

    @patch.dict(os.environ, {}, clear=True)
    def test_tmux_terminal_enriches_exact_conversation_on_local_host(self):
        with patch.object(compare, "tmux_identity", return_value={"terminalId": "ass-one"}), patch.object(compare, "request_api", side_effect=[
            {"state": "idle", "host": None},
            [{"id": "ass-two", "cwd": "/repo", "agent": "codex", "sessionId": "wrong"},
             {"id": "ass-one", "cwd": "/repo", "agent": "codex", "sessionId": "right"}],
        ]):
            result = compare.associate("http://fixture", {"repository": "/repo"}, args(title="Account layout"))
        self.assertEqual(result["artifact"], {"title": "Account layout", "owner": {
            "hostId": "local", "terminalId": "ass-one", "provider": "codex", "conversationId": "right",
        }})

    @patch.dict(os.environ, {}, clear=True)
    def test_remote_selection_cannot_enrich_a_local_owner(self):
        with patch.object(compare, "tmux_identity", return_value={"terminalId": "ass-one"}), patch.object(compare, "request_api", return_value={"state": "connected", "host": "remote"}) as request:
            result = compare.associate("http://fixture", {"repository": "/repo"}, args())
        self.assertNotIn("conversationId", result["artifact"]["owner"])
        request.assert_called_once_with("http://fixture", "remote/status")

    @patch.dict(os.environ, {"SSH_CONNECTION": "fixture"}, clear=True)
    def test_ssh_requires_explicit_host_and_keeps_supplied_identity(self):
        with patch.object(compare, "tmux_identity", return_value={"terminalId": "ass-one"}):
            with self.assertRaisesRegex(RuntimeError, "--host"):
                compare.associate("http://fixture", {"repository": "/repo"}, args())
        owner = {"hostId": "workstation", "terminalId": "ass-one", "provider": "codex", "conversationId": "saved"}
        with patch.object(compare, "request_api", side_effect=RuntimeError("offline")):
            result = compare.associate("http://fixture", {"repository": "/repo", "artifact": {"title": "Review", "owner": owner}}, args())
        self.assertEqual(result["artifact"]["owner"], owner)

    @patch.dict(os.environ, {}, clear=True)
    def test_explicit_terminal_does_not_inherit_invoking_conversation(self):
        with patch.object(compare, "tmux_identity", return_value={"terminalId": "ass-one", "provider": "codex", "conversationId": "old"}), patch.object(compare, "request_api", side_effect=RuntimeError("offline")):
            result = compare.associate("http://fixture", {"repository": "/repo"}, args(session="ass-two"))
        self.assertEqual(result["artifact"]["owner"], {"hostId": "local", "terminalId": "ass-two"})


    @patch.dict(os.environ, {}, clear=True)
    def test_terminal_override_retains_explicit_config_host(self):
        owner = {"hostId": "workstation", "terminalId": "ass-one", "provider": "codex", "conversationId": "old"}
        with patch.object(compare, "request_api", side_effect=RuntimeError("offline")):
            result = compare.associate("http://fixture", {"repository": "/repo", "artifact": {"title": "Review", "owner": owner}}, args(session="ass-two"))
        self.assertEqual(result["artifact"]["owner"], {"hostId": "workstation", "terminalId": "ass-two"})


if __name__ == "__main__":
    unittest.main()
