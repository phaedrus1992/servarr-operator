# Downloader Epic — Index and Shared Contracts

**Date:** 2026-07-26
**Epic:** [#366](https://github.com/phaedrus1992/servarr-operator/issues/366) — yt-dlp + gamdl download support
**Children:** [#363](https://github.com/phaedrus1992/servarr-operator/issues/363), [#364](https://github.com/phaedrus1992/servarr-operator/issues/364), [#365](https://github.com/phaedrus1992/servarr-operator/issues/365), [#379](https://github.com/phaedrus1992/servarr-operator/issues/379), [#362](https://github.com/phaedrus1992/servarr-operator/issues/362)
**Milestones:** v2.0.0 (all except #362, which is v1.3.0)
**Status:** Approved design, not yet implemented

---

## How to use this document

This is the index for a family of specs, one per issue. `dev-sprint` matches a spec
to selected work by issue number, so pick up the per-issue spec — not this file — when
implementing. This file holds only the parts that would otherwise be duplicated across
all of them: the dependency order and the cross-service contracts that two or more
issues both have to honour.

| Issue | Spec | Milestone | Depends on |
|-------|------|-----------|------------|
| #363 | [`2026-07-26-arr-registration-api-design.md`](2026-07-26-arr-registration-api-design.md) | v2.0.0 | — |
| #364 | [`2026-07-26-servarr-downloader-service-design.md`](2026-07-26-servarr-downloader-service-design.md) | v2.0.0 | — |
| #365 | [`2026-07-26-downloader-apptype-design.md`](2026-07-26-downloader-apptype-design.md) | v2.0.0 | #363, #364 |
| #379 | [`2026-07-26-gamdl-backend-design.md`](2026-07-26-gamdl-backend-design.md) | v2.0.0 | #364, #365 |
| #362 | none — mechanical deletion, the issue body is sufficient | v1.3.0 | — |

Build order: #363 and #364 in parallel (neither depends on the other), then #365, then
#379. #362 is independent cleanup and can land at any point; it ships on the v1.3.0 line
while everything else targets v2.0.0, so it must not be bundled into a v2.0.0 sprint.

---

## What the feature is

Let Sonarr, Radarr, and Lidarr download from YouTube, Vimeo, Apple Music, and anything
else `yt-dlp` or `gamdl` supports, by presenting those sources through the two protocols
the \*arr apps already speak: a Newznab indexer for search and a SABnzbd download client
for grab, progress, and import. A new companion service (`servarr-downloader`, #364)
implements both protocols and runs the download tools underneath. The operator deploys it
and wires it into every \*arr in the namespace (#365) using a registration API that does
not exist in the operator today (#363).

The same registration API fixes a standing gap unrelated to yt-dlp: the operator deploys
SABnzbd and Transmission next to the \*arr apps but has never connected them, so users
still add download clients by hand.

---

## Design decisions

These were settled during design and supersede the corresponding passages in the issue
bodies where they conflict. Each per-issue spec restates the ones it depends on.

### 1. Matching is metadata-driven with a confidence score

The original issue text assumed the indexer could answer a free-text \*arr query by
running a `yt-dlp` search. It cannot do that safely. Sonarr asks
`t=tvsearch&tvdbid=354629&season=1&ep=1`; a naive `ytsearch` returns whatever YouTube
feels like, and if the service dresses that in a synthetic `Show.S01E01.1080p...` title,
Sonarr auto-grabs an unrelated video and imports it as a real episode.

Instead: resolve the ID the \*arr sent into canonical metadata, score every candidate
against that metadata, and return only candidates above a configurable confidence floor.
Prior art for the scoring model is [Angrido/Lidarr-YouTube-Downloader](https://github.com/Angrido/Lidarr-YouTube-Downloader)
— which is, notably, the same tool the sidecar being deleted in #362 wraps.

### 2. Canonical metadata comes from the \*arr that asked

Rather than integrating TVDB, TMDB, and MusicBrainz clients (three API surfaces, two
credentials, rate limits, caching), the service calls back into the requesting \*arr to
resolve the ID it was given. The operator already holds every \*arr's URL and API key and
injects them, so this needs no new credentials and, by construction, the metadata matches
what the requesting app believes.

This requires the service to know *which* \*arr is calling, so registration is per-app:
the operator registers indexer URL `http://<svc>.<ns>.svc:8080/arr/<slug>/api` into each
\*arr, where `<slug>` identifies an entry in the service's `arrs` config.

### 3. Remux, never re-encode; advertise only what exists

The service probes each source's genuinely available formats and advertises only the
quality rungs it can actually deliver. On grab it selects the closest format and uses
`ffmpeg -c copy` to put the streams in the advertised container. It never re-encodes.
Quality profiles work because the advertised name describes what the file truly is.

The alternative in the original #364 text — advertise a full ladder and transcode to
match — burns significant CPU per grab, loses a generation of quality, and for audio
actively misinforms Lidarr, since YouTube Opus at 128k re-encoded to "320 MP3" contains
no more information than the 128k source did.

### 4. YouTube bot detection is a first-class deployment concern

`yt-dlp` from a datacenter IP reliably hits "Sign in to confirm you're not a bot".
YouTube is enforcing GVS PO tokens across most player clients
([yt-dlp PO Token Guide](https://github.com/yt-dlp/yt-dlp/wiki/PO-Token-Guide)).
Cookies alone no longer suffice. The Downloader pod therefore runs
`brainicism/bgutil-ytdlp-pot-provider` as a sidecar and reaches it over localhost, with an
optional cookies Secret layered on for age-restricted or members-only content. None of the
original issues mention this; without it the headline use case does not work unattended.

---

## Shared contracts

Two or more issues depend on each of these. Change them here, not in a per-issue spec.

### Downloads path identity

Sonarr, Radarr, and Lidarr import from the absolute `storage` path the download client
reports in its SABnzbd history, and that exact string must resolve inside the \*arr
container. The operator mounts the shared downloads PVC at the same `mountPath` across
every app, so the common case holds — but it is a real constraint, not an implementation
detail, and both #363 and #365 must document it. Remote path mapping is explicitly out of
scope; if a user mounts the downloads volume at differing paths, imports fail and the fix
is to align the mounts.

### Operator-managed registration naming

Every download client and indexer entry the operator creates in an \*arr is named
`servarr-operator/<cr-name>`. The `auto_remove` logic matches on that prefix so it only
ever deletes entries it owns, and never touches a hand-added client. Both #363 and #365
use this scheme; a stale-entry sweep written for one must work for the other.

### Release title format

The synthetic Newznab release title carries provenance so a user can judge a marginal
match from the \*arr UI and from grab history:

```
<parseable release name>-[<backend>-<source id>][c<confidence 0-99>]
```

For example `The.Show.S01E03.1080p.WEB-DL.H.264-[yt-dQw4w9WgXcQ][c91]` or
`Artist.-.Album.WEB.OPUS-[yt-abc123][c88]`. Everything before the first bracket is what
the \*arr quality parser consumes; the bracketed suffix is inert to the parser and
survives into the download folder name and the import record. #364 produces these and
#379 must produce the same shape for Apple Music.

### API key

One operator-generated key in an operator-managed Secret authenticates every surface of
the downloader — Newznab `apikey` query param, SABnzbd `apikey` query param, and the
admin config API. #365 provisions it; #364 reads it from env or file.

---

## Out of scope

Deliberately excluded from this epic. File separately if wanted.

- Remote path mapping between download client and \*arr (`remotePathMappings`).
- Re-encoding transcode ladders.
- A web UI on the downloader service; its only interfaces are the two emulated protocols
  plus a small admin config API the operator drives.
- Standalone use of the service outside the operator. It is designed to be operator-wired;
  the reflect-back metadata model assumes an operator populates the `arrs` config.
