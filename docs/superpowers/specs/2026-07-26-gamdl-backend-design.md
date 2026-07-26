# gamdl Backend Design (Apple Music)

**Date:** 2026-07-26
**Issue:** [#379](https://github.com/phaedrus1992/servarr-operator/issues/379) — gamdl backend for the downloader companion service
**Epic:** [#366](https://github.com/phaedrus1992/servarr-operator/issues/366) · index: [`2026-07-26-downloader-epic-index.md`](2026-07-26-downloader-epic-index.md)
**Milestone:** v2.0.0
**Depends on:** [#364](https://github.com/phaedrus1992/servarr-operator/issues/364) (backend abstraction), [#365](https://github.com/phaedrus1992/servarr-operator/issues/365) (the AppType that deploys it)

---

## Scope

A second backend for `servarr-downloader`, alongside `yt-dlp`. Apple Music sources route
to `gamdl`; everything else stays on `yt-dlp`. There is no separate AppType, no separate
deployment, and — if #364's backend abstraction is right — no change to the Newznab or
SABnzbd layers. This is deliberately the smallest of the four issues, and if it turns out
not to be, that is a signal the abstraction in #364 leaked.

Lidarr is the only consumer. `gamdl` is audio-only, so there is no video path.

---

## What differs from the `yt-dlp` backend

**Authentication is mandatory and non-anonymous.** `yt-dlp` can search and fetch public
YouTube content without an account. `gamdl` needs Apple Music account credentials for
everything, including search. The backend therefore reports itself unavailable rather than
degraded when credentials are absent, and the service's `/health` stays ready as long as
`yt-dlp` works — one unconfigured backend must not take the pod down.

**No resolution ladder.** Output is audio only. Quality rungs are format and bitrate:
ALAC (lossless), AAC 256, and whatever else `gamdl` exposes for the account tier. Map
these onto Lidarr's quality tiers in the same config-driven table #364 introduces, as an
additional section rather than a parallel mechanism.

**The remux-only rule still applies.** Advertise the rungs the account can actually
retrieve — a free or unsubscribed account cannot fetch lossless, so do not offer it — and
never re-encode to manufacture a rung. An ALAC rung advertised to Lidarr must be ALAC that
came out of Apple Music.

**Matching is easier here.** Apple Music catalogue metadata is structured and reliable, so
title and duration comparison against the MusicBrainz metadata Lidarr supplies scores far
higher than YouTube's free-text titles do. The channel-authority signal has no analogue and
is dropped; renormalise the remaining weights rather than leaving a 0.15 hole. The
negative-keyword filter still earns its place — Apple Music is full of live versions,
radio edits, and deluxe re-recordings.

**No PO token concern.** The bgutil sidecar is a YouTube-specific mitigation and is
irrelevant to this backend.

---

## Work

### Backend dispatch

`backend/mod.rs` gains `Backend::Gamdl`. Selection is by source URL host: `music.apple.com`
routes to `gamdl`, everything else to `yt-dlp`. Search-time selection follows from which
backends are available and configured — with credentials present, a Lidarr `audio-search`
queries both backends and the candidate lists merge before scoring, so the highest-confidence
match wins regardless of source.

The job record's `backend` field already exists from #364. Nothing in `newznab/`,
`sabnzbd/`, or `jobs/` should need editing; if it does, fix the abstraction rather than
special-casing.

### Credentials

`gamdl` reads a cookies file exported from a logged-in Apple Music session. It mounts from
a Secret referenced by `DownloaderConfig::apple_music_secret`, at a path `gamdl` expects
under `/config`. Never baked into the image, never in a ConfigMap.

Expired cookies are the predictable failure. Detect the auth failure specifically and
surface it as a distinct `fail_message` in SABnzbd history — "Apple Music credentials
expired or invalid" — so the user knows to re-export rather than debugging the download
path. Report it on the admin health detail too.

### Image

Pin `gamdl` alongside `yt-dlp` and `ffmpeg` in the #364 Dockerfile, with its own Renovate
rule. `gamdl` is Python, so it shares the interpreter `yt-dlp` already needs; confirm the
two do not pin conflicting dependency versions, and if they do, install them into separate
virtualenvs rather than resolving the conflict by unpinning either.

### Release titles

Identical shape to the shared contract, with `am` as the backend token:
`Artist.-.Album.WEB.ALAC-[am-1440857781][c94]`.

---

## Tests

- Backend dispatch: an `music.apple.com` URL selects `gamdl`, a YouTube URL selects
  `yt-dlp`, and an unknown host falls back to `yt-dlp`.
- The backend reports unavailable without credentials, and `/health` stays ready.
- Quality mapping: `gamdl` output formats map to the expected Lidarr tiers; an account
  without lossless does not get an ALAC rung advertised.
- Scoring: weights renormalise correctly with the channel signal absent.
- Auth failure produces the distinct credential-expiry `fail_message`, not a generic error.
- Merged candidate lists from both backends sort by confidence.

`cargo clippy --all-targets --all-features -- -D warnings` clean.

---

## Acceptance criteria

- [ ] `gamdl` is a selectable backend in `servarr-downloader`'s job queue; Apple Music
      sources route to it automatically.
- [ ] Apple Music credentials are read from a mounted Secret, not baked into the image.
- [ ] A grab against an Apple Music URL completes with a `Completed` SABnzbd history entry
      and an importable audio file at `storage`.
- [ ] Adding the backend required no change to the Newznab or SABnzbd layers.
- [ ] Only rungs the account can retrieve are advertised; no re-encoding.
- [ ] Expired credentials surface as a distinct, actionable failure message.
- [ ] Tests cover backend dispatch and the gamdl quality mapping.
- [ ] Docs note the Apple Music credential requirement and that the backend is inert
      without it.
