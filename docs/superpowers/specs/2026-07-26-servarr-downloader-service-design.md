# servarr-downloader Companion Service Design

**Date:** 2026-07-26
**Issue:** [#364](https://github.com/phaedrus1992/servarr-operator/issues/364) — servarr-downloader companion service (Newznab + SABnzbd bridge for yt-dlp & gamdl)
**Epic:** [#366](https://github.com/phaedrus1992/servarr-operator/issues/366) · index: [`2026-07-26-downloader-epic-index.md`](2026-07-26-downloader-epic-index.md)
**Milestone:** v2.0.0
**Depends on:** nothing in the operator
**Blocks:** [#365](https://github.com/phaedrus1992/servarr-operator/issues/365), [#379](https://github.com/phaedrus1992/servarr-operator/issues/379)

---

## What it is

A new crate, `crates/servarr-downloader`, producing its own binary and image. It speaks
two protocols the \*arr apps already know — Newznab for search, SABnzbd for download — and
runs `yt-dlp` and `ffmpeg` underneath. The operator is a controller and has no business
holding open a two-hour download, so this is a separate data-plane service with its own
storage.

The pattern is the one Quasarr uses. The closest working implementation of this specific
idea is [Angrido/Lidarr-YouTube-Downloader](https://github.com/Angrido/Lidarr-YouTube-Downloader),
which exposes Newznab at `/api/newznab/api` and SABnzbd at `/api/sabnzbd` and scores
YouTube candidates before accepting them. Its scoring model is the basis for the matching
engine below.

---

## Phases

The issue defines three, and they hold. Each is independently shippable and testable.

1. **Skeleton and the end-to-end loop.** HTTP surface, job queue, `yt-dlp` backend, a
   single quality rung, direct-URL resolution only (no metadata matching). Proves a grab
   flows from Sonarr through to an imported file.
2. **Matching and quality.** Metadata reflect-back, confidence scoring, multi-rung format
   probing, remux. This is the phase that makes automatic search safe.
3. **`gamdl` backend** — #379, additive on the same surface.

Phase 1 alone is a large sprint. Do not attempt 1 and 2 together.

---

## Module layout

One responsibility each, so a task can be held in context whole.

```
crates/servarr-downloader/src/
  main.rs        binary entry, config load, router assembly, graceful shutdown
  config.rs      file + env config, hot-reloadable arrs table
  newznab/       caps XML, search dispatch, RSS item serialization
  sabnzbd/       mode dispatch, queue/history JSON shapes
  admin.rs       operator-facing config API (the arrs table, health detail)
  metadata/      reflect-back clients: sonarr, radarr, lidarr
  matching/      candidate scoring, confidence, negative-keyword filter
  quality/       format probe, rung selection, synthetic release names
  backend/       trait + ytdlp impl (gamdl joins here in #379)
  jobs/          queue, state store, progress, lifecycle
```

**HTTP framework: axum.** The workspace is already on tokio; axum is tokio-native, is the
thinnest reasonable layer over hyper, and needs no runtime of its own. Raw hyper would
mean hand-rolling routing and extraction for a dozen endpoints. Nothing heavier is
warranted for a service with two emulated protocols and an admin endpoint.

---

## Component 1 — Newznab indexer

Mounted per-\*arr at `/arr/<slug>/api`, so the service knows which app is asking and can
resolve IDs against it. `<slug>` keys into the `arrs` config table.

### `t=caps`

Advertise only what the service can genuinely match on. The Torznab spec is explicit that
an implementation should return an empty result set for a parameter it does not support,
so honest caps plus empty results is the correct safe behaviour, not a degradation.

```xml
<tv-search    available="yes" supportedParams="q,tvdbid,season,ep" />
<movie-search available="yes" supportedParams="q,imdbid,tmdbid" />
<audio-search available="yes" supportedParams="q,artist,album" />
```

Categories: 2000 Movies, 3000 Audio, 5000 TV.

### `t=search|tvsearch|movie|music`

1. Resolve the incoming ID to canonical metadata via the requesting \*arr (below). If the
   query carries no ID and no usable `q`, return an empty feed.
2. Ask the backend for candidates for that metadata.
3. Score each candidate; drop everything below `min_confidence`.
4. Probe the survivors' real available formats; emit one `<item>` per deliverable rung.
5. Sort by confidence descending, then quality descending.

Each `<item>` carries a `<guid>` that is an opaque server-side key into a pending-release
table, and an `<enclosure url="…/download?token=<guid>">` pointing back at this service.
The token is a lookup key, not an encoding of the source URL and format — that keeps the
URL short, keeps the parameters out of \*arr logs, and makes the grab untamperable. Entries
are TTL'd (default 24h) and swept.

Auth is the `apikey` query param on every route, matching the SABnzbd convention.

---

## Component 2 — SABnzbd download client

Mounted at `/api/sabnzbd`. Implements exactly the modes an \*arr calls:

| Mode | Behaviour |
|------|-----------|
| `version` | `{"version": "4.5.5"}` — the \*arr apps gate features on this, so report a version SABnzbd actually shipped |
| `get_config` / `get_cats` | expose configured categories so the \*arr dropdown populates |
| `addurl`, `addfile` | resolve the token, enqueue a job, return `{"status": true, "nzo_ids": ["dl_…"]}` |
| `queue&output=json` | `slots[]` with `nzo_id`, `filename`, `cat`, `status`, `mb`, `mbleft`, `percentage`, `timeleft` |
| `history&output=json` | `slots[]` with `nzo_id`, `name`, `category`, `status`, `storage`, `fail_message` |
| `queue`/`history` + `name=delete&value=<nzo_id>` | cancel or remove |

`storage` is the completed job's absolute output directory and must be byte-identical to
what the \*arr container sees. See the path contract in the epic index.

Completed output lands at `<downloads_root>/<category>/<release title>/`, so the
provenance suffix in the release title survives into the folder name and shows up in the
\*arr import record.

---

## Component 3 — metadata reflect-back

The \*arr apps send IDs, never human-readable metadata. To score a candidate the service
needs the episode title and runtime, or the track title and length. It gets them by asking
the app that asked it.

Config table, populated by the operator through the admin API:

```toml
[[arrs]]
slug = "sonarr-main"
kind = "sonarr"
base_url = "http://sonarr.media.svc:8989"
api_key = "…"
```

| Kind | Lookup |
|------|--------|
| Sonarr | `GET /api/v3/series?tvdbId=` → series id, title; `GET /api/v3/episode?seriesId=&seasonNumber=&episodeNumber=` → episode title, runtime, air date |
| Radarr | `GET /api/v3/movie?tmdbId=` → title, year, runtime |
| Lidarr | `GET /api/v1/album?foreignAlbumId=<mbid>` → album title, artist, track list with durations |

Responses are cached in memory with a short TTL (default 15 minutes) so a season search
does not issue one lookup per episode against the same series.

If the lookup fails the service returns an empty feed and logs the reason. It never falls
back to guessing — an unresolvable ID is exactly the case where a wrong answer gets
imported as if it were right.

---

## Component 4 — confidence scoring

Weights are config-driven with these defaults, normalised to 0.0–1.0:

| Signal | Weight | Detail |
|--------|--------|--------|
| Title similarity | 0.45 | token-set ratio against the canonical title, after stripping bracketed noise, "official video", and channel-name prefixes |
| Duration match | 0.30 | 1.0 within 3s of the canonical runtime, decaying linearly to 0.0 at 10% deviation |
| Channel authority | 0.15 | 1.0 for a MusicBrainz-linked "Artist - Topic" channel, a verified official channel, or a user-mapped channel; 0.5 otherwise |
| Year match | 0.10 | movies only; exact year 1.0, ±1 year 0.5 |

A negative-keyword filter runs first and hard-rejects rather than scoring: `live`, `cover`,
`remix`, `karaoke`, `reaction`, `trailer`, `teaser`, `lyrics`, `8d audio`, `sped up`,
`nightcore`. Configurable, and per-request overridable when the canonical title itself
contains one of them (a track genuinely called "Live and Let Die" must not be filtered by
the `live` rule — match on word boundaries against the *difference* between candidate and
canonical title, not against the raw candidate).

YouTube's auto-generated "Artist - Topic" channels are the strongest single signal
available for music and should be treated as near-authoritative.

Only candidates at or above `min_confidence` (default 0.75) are returned at all. The score
is stamped into the release title as `[c<NN>]` so a user can judge a borderline result from
the interactive search view and from grab history.

### Post-download verification (audio)

Optional, off unless an AcoustID API key is configured. After a music download completes,
`fpcalc` fingerprints the file and the result is looked up against AcoustID; if the
returned recording MBID does not match the one Lidarr asked for, the job fails with an
explanatory `fail_message` instead of importing. This is the only signal that catches a
wrong-but-plausible match after the fact, and it is cheap.

---

## Component 5 — quality, remux only

Probe the source's real formats with `yt-dlp -J`. Map them onto \*arr quality rungs through
one config-driven table — resolution and codec for video, format and bitrate for audio.
**Advertise only rungs the source can actually deliver.** If only 720p exists, 1080p is
never offered, so a grab can never fail for a rung that was never there.

On grab: `yt-dlp -f <selector>` for the chosen rung, then `ffmpeg -c copy` to land the
streams in the advertised container. No re-encoding, ever. The advertised name therefore
describes the file truthfully and the \*arr quality profile behaves normally.

Release title format is the shared contract from the epic index:
`<parseable name>-[<backend>-<source id>][c<NN>]`.

The name encoder and its parser are a matched pair and get a round-trip property test.

---

## Service internals

- **State store: a single JSON file under `/config`, written atomically via temp-file plus
  rename, with the live state behind an `RwLock`.** Jobs number in the tens. SQLite would
  add a C dependency and a migration story to solve a problem this service does not have.
  Mark it with a `ponytail:` comment naming the ceiling and the upgrade path.
- Jobs survive restart. Anything found in `Fetching` on startup is resumed if the partial
  output is intact, otherwise re-queued. Job states: `Queued → Fetching → Postprocessing →
  Completed | Failed`.
- Each job record carries a `backend` field from the start, so #379 plugs in without
  touching the protocol layers.
- `yt-dlp` and `ffmpeg` are subprocesses. Progress comes from parsing `yt-dlp`'s
  `--progress-template` output, which is stable and machine-readable — do not scrape the
  human progress bar.
- Concurrency is capped (default 2 simultaneous jobs) because these are network- and
  disk-heavy and YouTube rate-limits aggressively.
- Structured `tracing` logging throughout. Never `println!`.
- `/health` reports ready only once the configured backends are actually runnable —
  `yt-dlp --version` and `ffmpeg -version` succeed. It deliberately does **not** depend on
  the \*arr apps being reachable, because the Downloader is tier 1 and starts before them.

### Failure handling

Every job failure records a `fail_message` that reaches SABnzbd history, so the user sees
the real reason in the \*arr queue rather than a silent disappearance. Distinguish at
minimum: bot-detection block, source unavailable or geo-restricted, no format satisfying
the requested rung, fingerprint mismatch, and disk full. Bot-detection in particular must
be named explicitly — it is the single most likely failure in a cluster deployment and a
generic "download failed" sends users hunting in the wrong place.

---

## Image and CI

Dockerfile pinning `yt-dlp`, `ffmpeg`, and `fpcalc` by version, running as `nonroot`.
Published as `ghcr.io/phaedrus1992/servarr-downloader` by a CI job mirroring
`.github/workflows/_publish.yaml`, which currently hardcodes a single `IMAGE_NAME` and
needs parameterising or a sibling job.

`yt-dlp` needs a much faster update cadence than the operator — YouTube breaks extractors
routinely — so the pin gets its own Renovate rule rather than riding the operator release
train.

The bot-detection story (the bgutil PO-token sidecar) is pod composition and belongs to
#365. This service only needs to read a PO-token provider URL and an optional cookies file
path from config and pass them through to `yt-dlp`.

---

## Tests

- **HTTP surface** — axum test harness over both protocol halves. Newznab caps validates
  against the schema; search returns well-formed RSS; every SABnzbd mode returns the shape
  the \*arr apps parse. Include a recorded real Sonarr query as a fixture.
- **Matching** — table-driven cases per signal, and the whole scorer against a fixture set
  of real YouTube result metadata with expected accept/reject verdicts. Explicitly cover
  the "Live and Let Die" negative-keyword false positive.
- **Quality** — property test for release-name encode/parse round-trip; a probe fixture
  where only 720p exists asserts no 1080p rung is advertised.
- **Jobs** — lifecycle transitions; a state file written and reloaded across a simulated
  restart; a job in `Fetching` at startup is resumed or re-queued, never lost.
- **Metadata** — wiremock each \*arr lookup, including the failure path returning an empty
  feed rather than a guess.
- **Boundary mocking only.** Mock the network, the filesystem where it is the boundary, and
  the `yt-dlp`/`ffmpeg` subprocesses. Never mock the scorer or the encoder.

`cargo clippy --all-targets --all-features -- -D warnings` clean, per CI's 1.94.0.

---

## Acceptance criteria

- [ ] `crates/servarr-downloader` builds; clippy clean at CI's toolchain.
- [ ] Newznab `t=caps` and search return XML that Sonarr/Radarr/Lidarr accept as a Generic
      Newznab indexer, verified against a real \*arr or a recorded contract.
- [ ] SABnzbd `version`/`get_config`/`get_cats`/`addurl`/`queue`/`history`/`delete`
      implemented and covered; `history.storage` points at the downloads volume.
- [ ] A grab runs `yt-dlp`, reports progress via `queue`, and completes with a `Completed`
      history entry and an importable file at `storage`.
- [ ] Metadata reflect-back resolves tvdbid/tmdbid/MusicBrainz IDs against the requesting
      \*arr; an unresolvable ID yields an empty feed, never a guess.
- [ ] Candidates below `min_confidence` are not returned; returned titles carry the source
      ID and confidence suffix.
- [ ] Only genuinely deliverable quality rungs are advertised; output is remuxed, never
      re-encoded.
- [ ] Quality mapping and scoring weights are config-driven, not hardcoded.
- [ ] Job records carry a `backend` field; adding `gamdl` (#379) requires no change to the
      Newznab or SABnzbd layers.
- [ ] Job failures surface a specific `fail_message`, with bot-detection named distinctly.
- [ ] Dockerfile builds with pinned `yt-dlp`/`ffmpeg`/`fpcalc`; CI publishes the image.
