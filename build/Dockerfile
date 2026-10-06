FROM rust:1.99.0-trixie@sha256:3745c050d12adc738eff16ebfc81ed044bfb2cc27c6828850ff1666beb1c7a49
RUN curl --fail --location --retry 3 -o /tmp/sccache.tar.gz \
      https://github.com/mozilla/sccache/releases/download/v0.18.0/sccache-v0.18.0-x86_64-unknown-linux-musl.tar.gz \
    && echo '45f1447fbe231e3037bde351ef70677dd212216c8d62ae7ca409fecc4d6acc89  /tmp/sccache.tar.gz' | sha256sum --check \
    && tar -xzf /tmp/sccache.tar.gz -C /tmp \
    && install -m 755 /tmp/sccache-v0.18.0-x86_64-unknown-linux-musl/sccache /usr/local/bin/sccache \
    && rm -rf /tmp/sccache* \
    && rustup component add rustfmt clippy
ENV CARGO_HOME=/cache/cargo CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
    RUSTC_WRAPPER=/usr/local/bin/sccache \
    SCCACHE_REDIS_ENDPOINT=redis://enfour-compiler-cache:6379 \
    SCCACHE_REDIS_KEY_PREFIX=enfour-rust-v1:
WORKDIR /workspace
# ort-sys 2.0.0-rc.13 CPU distribution. Preserve the exact upstream cache layout
# so Cargo builds on the internal cache network never need Internet access.
ENV ORT_CACHE_DIR=/opt/ort-cache
RUN curl --fail --location --retry 3 -o /tmp/ort.tar.lzma2 \
      https://cdn.pyke.io/0/pyke:ort-rs/ms@1.28.0/x86_64-unknown-linux-gnu.tar.lzma2 \
    && echo 'e454f710f8a49f53aa5b4ff51e3454ae1835777e431c6c35c5255ce6f205fd68  /tmp/ort.tar.lzma2' | sha256sum --check \
    && mkdir -p /opt/ort-cache/dfbin/x86_64-unknown-linux-gnu/e454f710f8a49f53aa5b4ff51e3454ae1835777e431c6c35c5255ce6f205fd68 \
    && xz --decompress --format=raw --lzma2=dict=64MiB --stdout /tmp/ort.tar.lzma2 > /tmp/ort.tar \
    && tar -xf /tmp/ort.tar -C /opt/ort-cache/dfbin/x86_64-unknown-linux-gnu/e454f710f8a49f53aa5b4ff51e3454ae1835777e431c6c35c5255ce6f205fd68 \
    && rm /tmp/ort.tar /tmp/ort.tar.lzma2
COPY cargo-cached /usr/local/bin/cargo-cached
ENTRYPOINT ["sh", "/usr/local/bin/cargo-cached"]
CMD ["test", "--locked", "--offline"]
