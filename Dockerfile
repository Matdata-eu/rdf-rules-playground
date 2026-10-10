# syntax=docker/dockerfile:1

# Build stage: compile the release binary. Web assets are embedded at compile
# time, so the binary is all the runtime image needs.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
# Build the dependencies on their own first so code changes reuse that layer.
RUN mkdir src && echo 'fn main() {}' > src/main.rs && touch src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src
COPY src ./src
COPY assets ./assets
COPY examples ./examples
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

# Runtime stage: glibc and nothing else, running as an unprivileged user.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/shacl-af-playground /usr/local/bin/shacl-af-playground

# Serve the playground as a public website on port 8080. Every
# SHACL_AF_PLAYGROUND_* variable maps to a `serve` flag; see the README.
ENV SHACL_AF_PLAYGROUND_BIND=0.0.0.0:8080 \
    SHACL_AF_PLAYGROUND_PUBLIC=true
EXPOSE 8080
WORKDIR /data
ENTRYPOINT ["shacl-af-playground"]
CMD ["serve", "--no-open"]
