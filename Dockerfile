FROM --platform=$BUILDPLATFORM registry.access.redhat.com/ubi9/ubi:latest as base
RUN dnf install -y rust-toolset go-toolset clang-devel openssl-devel &&\
	cargo install --root /usr/local cargo-chef --locked

WORKDIR /build

FROM base AS plan
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM base AS build
COPY --from=plan /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --bin controller --release

FROM registry.access.redhat.com/ubi9/ubi-micro:latest
WORKDIR /run
COPY --from=build /build/target/release/controller /usr/local/bin/controller

ENTRYPOINT ["/usr/local/bin/controller"]
