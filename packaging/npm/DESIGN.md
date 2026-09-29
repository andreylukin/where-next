# npm / npx launcher (design, not published)

Goal: `npx where-next …` and `npm i -g where-next` for Node-based agent setups, without running
unaudited remote scripts.

## Shape

- A tiny `where-next` package with a JS shim (`bin/wn.js`) and **optional dependencies**, one per
  platform: `@where-next/darwin-arm64`, `@where-next/darwin-x64`, `@where-next/linux-x64-gnu`,
  `@where-next/linux-arm64-gnu`, `@where-next/win32-x64`. Each contains only the `wn` binary from
  the matching release archive, with `os`/`cpu` fields so npm installs exactly one.
- The shim resolves the installed platform package and `execFileSync`s its binary, forwarding
  argv, stdio and the exit code. No `postinstall` download, no network access at install time.
- Package versions equal the release tag; binaries are copied from the attested release archives
  and their SHA-256 is recorded in each package's `package.json` (`wnSha256`). The shim verifies
  the hash once and caches the result next to the binary.

## Publishing (later)

A `publish-npm` job in the release workflow, tags only, with npm provenance (`npm publish
--provenance`), after the GitHub release is out of draft. Until then nothing is published.

## Not doing

- `postinstall` scripts that download binaries (blocked in many CI setups, and harder to audit).
- Bundling model weights. Models are fetched separately with `wn model pull` and have their own
  license terms.
