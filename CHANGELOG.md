# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
