# Benchmark/accuracy image for the minimal nginx and httpd injection modules.
# Build context: the repository root.
#   docker build -f bench/Dockerfile -t rum-inject-bench .
ARG NGINX_VERSION=1.28.0

FROM rust:1.94-bookworm AS base
ARG NGINX_VERSION
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        libclang-dev clang libpcre2-dev zlib1g-dev libssl-dev pkg-config \
        apache2 apache2-dev \
        wrk curl python3 procps util-linux linux-perf \
    && rm -rf /var/lib/apt/lists/*
# nginx from source: the module is built against this exact configure.
RUN curl -fsSL https://nginx.org/download/nginx-${NGINX_VERSION}.tar.gz | tar xz -C /opt \
    && cd /opt/nginx-${NGINX_VERSION} \
    && ./configure --prefix=/opt/nginx --with-compat --with-threads --with-http_ssl_module \
    && make -j"$(nproc)" && make install
ENV NGINX_SOURCE_DIR=/opt/nginx-${NGINX_VERSION} \
    NGINX_BUILD_DIR=/opt/nginx-${NGINX_VERSION}/objs \
    PATH=/opt/nginx/sbin:$PATH
# Apache: stop the distro service config from getting in the way; we run our own conf.
RUN a2dismod -f mpm_prefork mpm_worker >/dev/null 2>&1 || true

FROM base AS build
COPY . /src
# The repo pins a toolchain with Windows targets; the image toolchain is enough here.
RUN rm -f /src/rust-toolchain.toml
WORKDIR /src/bench
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/bench/target \
    cargo build --release \
    && mkdir -p /opt/rum && cp target/release/libngx_http_rum_inject_module.so \
       target/release/libmod_rum_inject.so target/release/oracle /opt/rum/
ENTRYPOINT ["/src/bench/run.sh"]
