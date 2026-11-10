# Governance

`sbx` is an open-source project of [toollyst](https://github.com/toollyst), licensed Apache-2.0 OR MIT.

## Roles

- **Contributors** open issues and pull requests. Anyone can.
- **Maintainers** ([MAINTAINERS.md](MAINTAINERS.md)) review and merge changes, cut releases and handle security
  reports. A maintainer's own change is merged after review by another maintainer once there are two or more.

## Decisions

Most decisions happen in pull requests and issues. Changes to a public contract (CLI flags and exit codes, JSON
output, the MCP tools, the executor and guest protocols, the repository index format, the on-disk layout) need an
issue first and a note in the roadmap ([docs/roadmap.md](docs/roadmap.md)). When maintainers disagree, they try to
reach consensus; if they can't, the majority of maintainers decides.

## Becoming a maintainer

A contributor who has made sustained, good contributions and reviews can be nominated by a maintainer. The
current maintainers decide together, and the change is a pull request to MAINTAINERS.md. A maintainer who has been
inactive for a year, or who asks to step down, moves to an emeritus list.

## Security

Security reports follow [SECURITY.md](SECURITY.md): they are handled privately by the maintainers, with the response
times stated there.

## Code of conduct

Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).
