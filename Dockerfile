FROM rust:bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 65532 --create-home vibe
COPY --from=build /src/target/release/vibe /usr/local/bin/vibe
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["vibe"]
CMD ["serve", "--addr", "0.0.0.0:8080"]
