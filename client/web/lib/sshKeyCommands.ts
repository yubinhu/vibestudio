/** Commands to run on the SSH host as the connection's login user. Explicitly
 * using sh also supports users whose login shell is fish, and isolates umask. */
export function sshKeyInstallCommands(publicKey: string): string {
  const key = publicKey.trim();
  // A key must be one authorized_keys entry. In particular, never copy control
  // characters into a terminal or allow a comment to add another key line.
  if (
    !/^ssh-ed25519 [A-Za-z0-9+/]+={0,2}(?: .*)?$/.test(key) ||
    [...key].some((character) => character.charCodeAt(0) < 32 || character.charCodeAt(0) === 127)
  ) {
    throw new Error("The generated public key is invalid. Generate a new key and try again.");
  }
  // Leave single quotes AND backslashes outside quoted segments: fish interprets
  // \\ and \' inside single quotes, whereas POSIX shells leave them literal.
  // Every other character stays quoted, including dollars, backticks and !.
  const quotedKey = `'${key.replace(/[\\']/g, (character) => `'\\${character}'`)}'`;
  return [
    "sh -eu -c '",
    "  umask 077",
    '  mkdir -p "$HOME/.ssh"',
    '  chmod 700 "$HOME/.ssh"',
    '  touch "$HOME/.ssh/authorized_keys"',
    '  chmod 600 "$HOME/.ssh/authorized_keys"',
    '  if grep -qxF "$1" "$HOME/.ssh/authorized_keys"; then',
    "    exit 0",
    "  else",
    // grep status 1 means no match; an actual read/command error must not append.
    '    [ "$?" -eq 1 ] || exit 1',
    "  fi",
    // Start with a newline so an existing key without a final newline survives.
    '  printf "\\n%s\\n" "$1" >> "$HOME/.ssh/authorized_keys"',
    `' vibestudio-ssh-key ${quotedKey}`,
  ].join("\n");
}
