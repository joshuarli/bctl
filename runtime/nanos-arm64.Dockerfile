FROM debian:bookworm@sha256:268fb24c39bca92d326dd83ea3abebe4a3f7751c804ffef23bb6d857dd343ff8
RUN apt-get update -qq && apt-get install -y -qq build-essential ca-certificates ccache fd-find git xxd \
    && ln -s /usr/bin/fdfind /usr/local/bin/fd \
    && rm -rf /var/lib/apt/lists/*
