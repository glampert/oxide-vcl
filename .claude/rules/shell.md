# Shell

The Bash tool runs zsh, not bash:

- Unquoted `$var` does not word-split: `p="a b"; set -- $p` leaves `$1` as
  `a b`. Use `${=p}` or an array when splitting is wanted.
- A command that starts with `=` (for example `===`) is an `=cmd` expansion
  error. Print separators with `echo "==="`.
- Never `rm -rf` or `cp` into a path that ends in a variable that could be
  empty. An empty variable once deleted a whole evidence directory; since
  then `local/` holds data with no git copy, so the loss would be permanent.
  Use literal paths, or check the variable first.
- On macOS file names are case-insensitive: `vf-r.vcl` and `vf-R.vcl` are the
  same file.
- There is no `timeout` command; use the tool's own `--timeout` options.
