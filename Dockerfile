# Builds a static musl binary and ships it on gcr.io/distroless/static-debian12:nonroot —
# the same base image _publish.yaml's release build uses — so this Dockerfile (and anything
# built from it, including the CI smoke test) validates the artifact that actually ships,
# not a different debian:bookworm-slim/glibc image nothing else uses.
#
# No cargo-chef: its whole point is skipping dependency recompilation via a separate
# "cook the deps, then build the source" Docker layer. The --mount=type=cache mounts below
# already give that same incremental-build benefit (BuildKit persists them across builds on
# a given builder instance, independent of image layers), so cargo-chef's extra stages and
# its own separately-versioned base image would be redundant complexity here. It also let
# the build silently run rustc 1.93 while rust-toolchain.toml pins 1.98.0 — using the
# official rust image with an explicit tag keeps this in sync with that pin instead.
FROM rust@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922 AS builder
# ^ rust:1.98.0-bookworm (multi-arch index digest, covers amd64 + arm64) — keep in sync
# with rust-toolchain.toml's pinned channel.
WORKDIR /build

# TARGETARCH is set by buildx to the platform actually being built for. A plain `docker
# build` (no --platform) builds natively for the host, so this never cross-compiles: an
# amd64 host's build container is amd64 and compiles for x86_64-unknown-linux-musl; an
# arm64 host's build container is arm64 and compiles for aarch64-unknown-linux-musl.
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools && apt-get clean \
    && case "$TARGETARCH" in \
         amd64) echo x86_64-unknown-linux-musl > /build/rust-target ;; \
         arm64) echo aarch64-unknown-linux-musl > /build/rust-target ;; \
         *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
       esac \
    && rustup target add "$(cat /build/rust-target)"

COPY Cargo.toml Cargo.lock image-defaults.toml ./
COPY crates crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --target "$(cat /build/rust-target)" --bin servarr-operator \
    && cp "/build/target/$(cat /build/rust-target)/release/servarr-operator" /build/servarr-operator

FROM gcr.io/distroless/static-debian12@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
# ^ static-debian12:nonroot (multi-arch index digest, covers amd64 + arm64)
COPY --from=builder /build/servarr-operator /servarr-operator
ENTRYPOINT ["/servarr-operator"]
