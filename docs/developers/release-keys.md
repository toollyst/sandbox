# Release signing keys

`sbx upgrade` installs only archives signed by an **archive key**, and the new-version notice reads only a
`releases.json` signed by an **index key** ([SECURITY.md](../../SECURITY.md#verifying-what-you-install)). The public
halves are compiled into `sbx` (`ARCHIVE_KEYS` and `INDEX_KEYS` in
[`update.rs`](../../crates/app/cli/src/update.rs)); while those lists are empty, `sbx` shows no notice and
`sbx upgrade` refuses to install anything.

This page is the one-time setup. You make four key pairs: one archive key in use and one spare, one index key in use
and one spare. The spares are compiled into `sbx` too, so you can switch to one later without a release that only
some users have.

## The short way: one script

```sh
.github/scripts/apply-settings.sh     # once, if the release-index environment doesn't exist yet
.github/scripts/release-keys.sh       # steps 1, 2, 4 and 5 below
```

[`release-keys.sh`](../../.github/scripts/release-keys.sh) builds xtask and makes the four pairs in `release-keys/` at
the repository root. That folder is in `.gitignore`, and the script refuses to run if git would not ignore it. Then it
uploads `archive-1` and `index-1` as environment secrets and writes the four public keys into `update.rs`. It doesn't
overwrite keys that are already in the folder, or secrets that are already set (`--force` replaces the secrets). With
`--no-upload` it leaves GitHub alone. Afterwards:

1. Back up the four `.key` files (step 3). Then you may delete `release-keys/`.
2. Commit `update.rs`. Run step 6 if you want to check the keys.

The script makes the keys on the machine you run it on, online. If you want them made offline, follow the steps
below by hand.

## What you need

- A checkout of this repository on a machine you trust, with Rust installed.
- `gh` logged in as a repository admin (to set environment secrets).
- Two places to keep secret keys offline, for example an encrypted USB drive and a password manager.

## 1. Build the tool while you are online

```sh
cd sandbox
git pull
cargo build -p xtask
```

From here on nothing needs the network until step 4. You can unplug it if you want the keys to be made offline.

## 2. Make the four key pairs

```sh
umask 077
mkdir -p ~/sbx-release-keys
cargo xtask repo-keygen --out ~/sbx-release-keys/archive-1
cargo xtask repo-keygen --out ~/sbx-release-keys/archive-2
cargo xtask repo-keygen --out ~/sbx-release-keys/index-1
cargo xtask repo-keygen --out ~/sbx-release-keys/index-2
ls -l ~/sbx-release-keys
```

Each command prints its public key and writes `<name>.pub` (public) and `<name>.key` (secret, mode 0600, no
password). It refuses to overwrite a file that exists. `repo-keygen` writes "sbx executor repository signing key" as
the key's comment. That comment is cosmetic: the same tool makes every minisign key the project uses.

## 3. Back up the secret keys

Copy all four `.key` files to both offline places. Never commit them, paste them into an issue or a chat, or keep
them in a synced folder. If you lose `archive-1.key` and `archive-2.key`, `sbx upgrade` can't move past the last
release they signed, and users have to run the installer again.

## 4. Put the keys in use into GitHub

Only `archive-1` and `index-1` go into GitHub. The spares (`*-2.key`) stay offline.

```sh
# Once: create the release-index environment (no approval, main only) and refresh the other settings.
.github/scripts/apply-settings.sh

gh secret set SBX_RELEASE_SIGNING_KEY --env release       --repo toollyst/sandbox < ~/sbx-release-keys/archive-1.key
gh secret set SBX_RELEASE_INDEX_KEY   --env release       --repo toollyst/sandbox < ~/sbx-release-keys/index-1.key
gh secret set SBX_RELEASE_INDEX_KEY   --env release-index --repo toollyst/sandbox < ~/sbx-release-keys/index-1.key
gh secret list --env release --repo toollyst/sandbox
gh secret list --env release-index --repo toollyst/sandbox
```

The archive key is only in `release`, which needs a maintainer's approval and runs only for `v*` tags. The index key
is also in `release-index`, so the weekly re-signing ([`releases-resign.yml`](../../.github/workflows/releases-resign.yml))
runs without anyone approving it. That job can only re-sign an index the index key already signed.

## 5. Compile the public keys into sbx

```sh
cat ~/sbx-release-keys/archive-1.pub ~/sbx-release-keys/archive-2.pub
cat ~/sbx-release-keys/index-1.pub ~/sbx-release-keys/index-2.pub
```

Put the lines into [`update.rs`](../../crates/app/cli/src/update.rs), the key in use first:

```rust
pub const INDEX_KEYS: &[&str] = &["RW…index-1…", "RW…index-2…"];
pub const ARCHIVE_KEYS: &[&str] = &["RW…archive-1…", "RW…archive-2…"];
```

Public keys are not secret: commit them, and list them in the release notes so people can check them.

## 6. Check it once

```sh
echo test > /tmp/sbx-key-check
cargo xtask sign-files --key ~/sbx-release-keys/archive-1.key /tmp/sbx-key-check
minisign -Vm /tmp/sbx-key-check -P "$(cat ~/sbx-release-keys/archive-1.pub)"   # if you have minisign
rm /tmp/sbx-key-check /tmp/sbx-key-check.minisig
```

The real check is the first tag after the keys are in. Its `publish` job signs the archives and creates
`releases.json`, and the log has no "is not set" warnings. After that, `sbx upgrade --check` on the previous release
reads the index.

## Switching keys

- **Planned:** put the spare into the secret (re-sign `releases.json` with it once, with `--signed-by` and the old
  public key, as below), then make a new spare and add it to the lists in the next release.
- **A secret key leaked:** put the spare into the secret at once and release with it. Then release an `sbx` without
  the leaked key, and say so under **Security** in the CHANGELOG. If it was the index key, also re-sign
  `releases.json` with the new key: `cargo xtask release-sign --index pages/releases.json --key index-2.key
  --signed-by "$(cat index-1.pub)"` (`--signed-by` names the key the current file was signed with). An `sbx` that still trusts the leaked index key can be
  shown fake notices until it upgrades, but it can't be made to install an archive the archive key didn't sign.
