# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased 

## Added

- Add support for Tautulli as a way to connect to Plex.
- New config option `exclude_tag` to ignore series by Sonarr tag.

## Changed

- Docker: No need to set `PREFETCHARR_CONFIG` when providing config as file.


## [1.2.0] - 2025-09-11

## Added

- Log level configurable via TOML instead of RUST_LOG variable.

## Fixed

- Correctly monitor seasons of mixed monitoring state. ([@jon4hz](https://github.com/jon4hz))


## [1.1.0] - 2025-08-05

## Added

- `request_seasons` can be set to `false` to request single episodes.

## Changed

- Configuration is done through a TOML file instead of environment variables and
  CLI parameters. This change is backwards compatible.

## Removed

- Unmaintained Unraid instructions and template.


## [1.0.0] - 2025-03-18

## Removed

- Remove deprecated `--jellyfin-url` and `JELLYFIN_API_KEY` options.


## [0.10.0] - 2025-02-16

## Added

- Optional allow list for media server libraries. 

## Changed

- TLS root certificates are no longer compiled in but provided by the platform.
  

## [0.9.0] - 2025-01-04

## Added

- Add an optional setting to retry the initial connection probing. This is
  always set by the Dockerfile.


## [0.8.2] - 2024-12-01

## Fixed

- Use the correct episode count from Sonarr to not consider monitored episodes
  as downloaded.

## Added

- Add a Docker template for Unraid. ([@f0rc3d](https://github.com/f0rc3d))


## [0.8.1] - 2024-10-31

## Fixed

- Plex sessions were not detected since 0.8.0.


## [0.8.0] - 2024-10-20

### Added

- Optional allow list for media server users. ([@aksiksi](https://github.com/aksiksi))


## [0.7.4] - 2024-10-07

### Changed

- Update dependencies (`futures-util` got yanked for soundness issues)


## [0.7.3] - 2024-05-27

### Fixed

- Fix an issue where the second season is not requested under specific
  circumstances.

### Changed

- Update dependencies.


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
