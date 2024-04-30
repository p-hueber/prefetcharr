# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.7.2] - 2024-04-30

### Added

- Add license text files.

### Changed

- Update dependencies.


## [0.7.1] - 2024-04-17

### Fixed

- Fix an authentication issue with Sonarr.


## [0.7.0] - 2024-04-14

### Added

- Trigger on pilot episodes.
- Probe all connections at startup.

### Changed

- Better reporting of errors returned by Sonarr or the media server.
- Put all API keys into request headers instead of URL queries and never log
  them.


## [0.6.0] - 2024-04-06

### Added

- Docker images are published to Docker Hub as `phueber/prefetcharr:latest`.

### Changed

- Always log to `stderr`, regardless of the `--log-dir` flag.
- Only log with ANSI colors if `stderr` is a terminal.
- Use rustls instead of openssl.
- Reduce binary and docker image size.


## [0.5.1] - 2024-04-03

### Changed

- Set the default log level to `INFO`.

### Fixed

- Skip over malformed series from Sonarr but search the rest.
- Treat the `statistics` field of a season in Sonarr as optional.


## [0.5.0] - 2024-04-02

### Added

- Add Plex support.
- Log version.

### Changed

- Search series by name if tvdb id is not available.


## [0.4.0] - 2024-03-15

### Added

- Specify a minimum supported rust version.
- Add Emby support.

### Changed

- Update dependendencies.
- CLI interface changed to accomodate Emby.

### Deprecated

- `--jellyfin-url`, `--jellyfin-api-key` and `JELLYFIN_API_KEY`


## [0.3.0] - 2024-01-07

### Added

- New command-line option `--remaining-episodes` to control when the next
  season is searched.
- Set the new "monitor new seasons" option of Sonarr v4 when watching the
  last season.

### Changed

- Dockerfile exposes all command-line arguments via environment variables.


## [0.2.0] - 2024-01-03

### Changed

- Process a given season only once. Remember this for seven days or until the
  program is restarted.
- Do not ignore seasons that are already monitored.
- Make sure the series is monitored when monitoring a season.
- Ignore seasons that were downloaded already.
