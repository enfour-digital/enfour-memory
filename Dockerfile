FROM debian:trixie-slim@sha256:a99cfc517144bc59b1978475ec53b46ecabec7e43635402ee5b77cc54cd1b20a
RUN apt-get update && apt-get install -y --no-install-recommends libstdc++6 ca-certificates git \
    && rm -rf /var/lib/apt/lists/*
COPY target/release/enfour-memory /usr/local/bin/enfour-memory
USER 1000:100
WORKDIR /data
ENV TOKENIZERS_PARALLELISM=false
EXPOSE 7463
ENTRYPOINT ["/usr/local/bin/enfour-memory", "--db", "/data/memory.sqlite", "--models", "/models"]
CMD ["serve", "--bind", "0.0.0.0:7463", "--token-file", "/data/access.token", "--hosts", "localhost,127.0.0.1"]
