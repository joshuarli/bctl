FROM debian:bookworm@sha256:268fb24c39bca92d326dd83ea3abebe4a3f7751c804ffef23bb6d857dd343ff8
RUN apt-get update -qq && apt-get install -y -qq build-essential ca-certificates ccache curl fd-find git xxd \
    && ln -s /usr/bin/fdfind /usr/local/bin/fd \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL -o /usr/local/bin/jaq https://github.com/01mf02/jaq/releases/download/v3.1.2/jaq-aarch64-unknown-linux-gnu \
    && echo '1aa1b9c0a3b73f016f6a8d407fa21a89174d01a8e4b9df449eaad49603426734  /usr/local/bin/jaq' | sha256sum -c - \
    && chmod +x /usr/local/bin/jaq \
    && git config --system safe.directory "*"
