# OSPS Baseline self-assessment

How this repository meets the [OpenSSF Open Source Project Security Baseline](https://baseline.openssf.org/versions/2025-10-10)
(version 2025-10-10), control by control. Status is one of **met**, **partly** (what is missing is said) or
**not met**. Last checked 2026-11-11 for v0.2.1. Update this file in the same pull request as a change that affects a
control.

Summary: every Level 1 and Level 2 control is met except MFA enforcement (OSPS-AC-01.01), which is an organisation
setting. At Level 3, three are not met or partly met: non-author review (OSPS-QA-07.01, needs a second maintainer),
VEX documents (OSPS-VM-04.02), and CodeQL blocking merges (OSPS-VM-06.02); subprojects (OSPS-QA-04.02) are partly met.

## Access control

| Control | Level | Status | Evidence |
| --- | --- | --- | --- |
| AC-01.01 MFA for sensitive resources | 1 | **not met** | The maintainer account uses 2FA, but the `toollyst` organisation doesn't require it. Turning the requirement on is a maintainer action in the organisation settings. |
| AC-02.01 lowest privileges for new collaborators | 1 | met | Organisation default permission is `read`; higher access only as in [GOVERNANCE.md](../../GOVERNANCE.md#becoming-a-maintainer). |
| AC-03.01 no direct commits to the primary branch | 1 | met | Ruleset `main` ([`.github/settings/ruleset-main.json`](../../.github/settings/ruleset-main.json)) requires a pull request and the `ci` checks. Admins can bypass it for urgent fixes. |
| AC-03.02 primary branch deletion protected | 1 | met | Same ruleset: deletion and force-push are blocked. |
| AC-04.01 CI defaults to lowest permissions | 2 | met | Repository default workflow token is read-only; every workflow sets `permissions: contents: read` (or `read-all`) at the top. |
| AC-04.02 jobs get only what they need | 3 | met | Write scopes (`id-token`, `attestations`, `security-events`, `contents: write` for publishing) are granted per job in [`release.yml`](../../.github/workflows/release.yml), [`sbx-build.yml`](../../.github/workflows/sbx-build.yml), [`codeql.yml`](../../.github/workflows/codeql.yml), [`scorecard.yml`](../../.github/workflows/scorecard.yml). |

## Build and release

| Control | Level | Status | Evidence |
| --- | --- | --- | --- |
| BR-01.01 CI inputs sanitized | 1 | met | Workflow inputs reach scripts only through `env:` (never `${{ }}` inside `run:`), and are checked (`fuzz.yml` checks target names against the target list). CodeQL scans the workflows. |
| BR-01.02 branch names sanitized | 1 | met | No workflow interpolates a branch or tag name into a script. `release.yml` reads the tag only from the environment (`$GITHUB_REF_NAME`, quoted) and refuses it unless it equals `v` + the version in `Cargo.toml`. |
| BR-02.01 unique version per release | 2 | met | SemVer tags `vX.Y.Z`; [COMPATIBILITY.md](../../COMPATIBILITY.md). |
| BR-02.02 assets tied to the release | 3 | met | Archive names carry the version and target; each has a SHA-256, a Sigstore bundle and attestations for that tag. |
| BR-03.01, BR-03.02 encrypted channels | 1 | met | GitHub over HTTPS; installers are served from GitHub Pages over HTTPS and download only HTTPS URLs. |
| BR-04.01 change log | 2 | met | [CHANGELOG.md](../../CHANGELOG.md) with a **Security** section per release. |
| BR-05.01 standard dependency tooling | 2 | met | Cargo with the committed `Cargo.lock`, `--locked` builds. |
| BR-06.01 signed releases | 2 | met | Sigstore keyless signatures, GitHub build provenance (SLSA Build L3) and SBOM attestations; [SECURITY.md](../../SECURITY.md#verifying-what-you-install). |
| BR-07.01 no secrets in version control | 1 | met | Secret scanning with push protection ([`.github/settings/security-and-analysis.json`](../../.github/settings/security-and-analysis.json)); `.gitignore` covers local keys. |
| BR-07.02 secrets policy | 3 | met | [GOVERNANCE.md](../../GOVERNANCE.md#secrets): no long-lived secrets, how one would be stored, who can read it, rotation. |

## Documentation

| Control | Level | Status | Evidence |
| --- | --- | --- | --- |
| DO-01.01 user guides | 1 | met | [docs/guide/](../guide/README.md), `sbx --help`. |
| DO-02.01 how to report defects | 1 | met | Issue forms ([`.github/ISSUE_TEMPLATE/`](../../.github/ISSUE_TEMPLATE)), [CONTRIBUTING.md](../../CONTRIBUTING.md). |
| DO-03.01 verifying integrity and authenticity | 3 | met | [SECURITY.md](../../SECURITY.md#verifying-what-you-install); the installers verify by default (`SBX_VERIFY`). |
| DO-03.02 verifying the release identity | 3 | met | Same section: `cosign verify-blob` with the expected workflow identity, `gh attestation verify --signer-workflow`. |
| DO-04.01 scope and duration of support | 3 | met | [SECURITY.md](../../SECURITY.md#reporting-a-vulnerability), "Supported versions". |
| DO-05.01 end of security updates | 3 | met | Same paragraph: latest two minors, at least a year each; 0.1.x withdrawn. |
| DO-06.01 dependency management | 2 | met | [CONTRIBUTING.md](../../CONTRIBUTING.md#dependencies), [`deny.toml`](../../deny.toml), [`.github/dependabot.yml`](../../.github/dependabot.yml). |

## Governance

| Control | Level | Status | Evidence |
| --- | --- | --- | --- |
| GV-01.01 members with sensitive access | 2 | met | [MAINTAINERS.md](../../MAINTAINERS.md) (who can release). |
| GV-01.02 roles and responsibilities | 2 | met | [GOVERNANCE.md](../../GOVERNANCE.md#roles). |
| GV-02.01 public discussion | 1 | met | Issues and pull requests. |
| GV-03.01 contribution process | 1 | met | [CONTRIBUTING.md](../../CONTRIBUTING.md). |
| GV-03.02 contributor guide and requirements | 2 | met | Same file: checks to run, tests, DCO, layering; [developers guide](README.md). |
| GV-04.01 review before escalated permissions | 3 | met | [GOVERNANCE.md](../../GOVERNANCE.md#becoming-a-maintainer). |

## Legal

| Control | Level | Status | Evidence |
| --- | --- | --- | --- |
| LE-01.01 legal authorization per commit | 2 | met | DCO sign-off, checked by the `dco` CI job on every pull request commit. |
| LE-02.01, LE-02.02 OSI licenses | 1 | met | Apache-2.0 OR MIT for the source and the released binaries. |
| LE-03.01, LE-03.02 license files | 1 | met | `LICENSE-APACHE`, `LICENSE-MIT` at the root and in every release archive. |

## Quality

| Control | Level | Status | Evidence |
| --- | --- | --- | --- |
| QA-01.01, QA-01.02 public repository and history | 1 | met | <https://github.com/toollyst/sandbox>; `main` can't be force-pushed. |
| QA-02.01 dependency list | 1 | met | `Cargo.toml`, `Cargo.lock`; [crates.md](crates.md). |
| QA-02.02 SBOM with compiled assets | 3 | met | SPDX SBOM attested per release; dependency list embedded in the binary (cargo-auditable). |
| QA-03.01 status checks before merge | 2 | met | Ruleset `main` requires `supply-chain`, `dco`, the test matrix, `privileged` and `vm`. |
| QA-04.01 subprojects listed | 1 | met | The executors and their repository live in [sandbox-executors](https://github.com/toollyst/sandbox-executors); the macOS VMM in [wick](https://github.com/toollyst/wick); `fuzz/` and `xtask/` are in this repository ([developers guide](README.md)). |
| QA-04.02 subprojects as strict | 3 | partly | `fuzz/` and `xtask/` follow the same CI. sandbox-executors and wick are separate projects with their own settings; `sbx` verifies what it installs from them (signed index, digests) instead of trusting them. |
| QA-05.01, QA-05.02 no generated or unreviewable binaries | 1 | met | No executables are committed. `fuzz/seeds/` are small inputs generated by [`fuzz/examples/seeds.rs`](../../fuzz/examples/seeds.rs); CI fails if they differ from the generator. |
| QA-06.01 automated tests before accepting | 2 | met | `ci.yml` runs on every pull request and is required. |
| QA-06.02 when and how tests run | 3 | met | [testing-and-releasing.md](testing-and-releasing.md). |
| QA-06.03 changes add tests | 3 | met | [CONTRIBUTING.md](../../CONTRIBUTING.md#pull-requests): "tests that fail without it". |
| QA-07.01 non-author approval | 3 | **not met** | One maintainer today; the ruleset requires no approval. [MAINTAINERS.md](../../MAINTAINERS.md) asks for a second; once there is one, set `required_approving_review_count` to 1. |

## Security assessment

| Control | Level | Status | Evidence |
| --- | --- | --- | --- |
| SA-01.01 design documentation | 2 | met | [how-it-works.md](how-it-works.md), [crates.md](crates.md). |
| SA-02.01 external interfaces | 2 | met | [COMPATIBILITY.md](../../COMPATIBILITY.md) lists every contract: CLI, [JSON output](cli-output.md), MCP tools, executor protocol, repository index, on-disk layout. |
| SA-03.01 security assessment | 2 | met | [SECURITY.md](../../SECURITY.md#threat-model) and two adversarial reviews with regression tests (roadmap P17, P18). |
| SA-03.02 threat model and attack surface | 3 | met | The threat model table names each boundary and what crosses it; fuzz targets cover the parsers on those boundaries ([Fuzzing](testing-and-releasing.md#fuzzing)). |

## Vulnerability management

| Control | Level | Status | Evidence |
| --- | --- | --- | --- |
| VM-01.01 disclosure policy with timeframes | 2 | met | [SECURITY.md](../../SECURITY.md#reporting-a-vulnerability). |
| VM-02.01 security contacts | 1 | met | Same section; MAINTAINERS.md. |
| VM-03.01 private reporting | 2 | met | GitHub private vulnerability reporting is enabled. |
| VM-04.01 publish vulnerability data | 2 | met | GitHub security advisories with CVEs, and the release notes' Security section. |
| VM-04.02 VEX for non-affecting vulnerabilities | 3 | **not met** | Advisories ignored in `deny.toml` carry the reason there, but no VEX document is published yet. |
| VM-05.01, VM-05.02 SCA thresholds and pre-release policy | 3 | met | [SECURITY.md](../../SECURITY.md#verifying-what-you-install), "Findings". |
| VM-05.03 block vulnerable dependencies automatically | 3 | met | `cargo deny check` in the required `supply-chain` job; Dependabot security updates. |
| VM-06.01 SAST threshold | 3 | met | SECURITY.md "Findings". |
| VM-06.02 automatic SAST on changes | 3 | partly | [`codeql.yml`](../../.github/workflows/codeql.yml) runs on every pull request and clippy with `-D warnings` is a required check. CodeQL alerts don't block a merge yet: after its first run on `main`, add a `code_scanning` rule (high or higher) to the `main` ruleset. |
