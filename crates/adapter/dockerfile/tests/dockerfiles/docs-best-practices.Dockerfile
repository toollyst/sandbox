FROM ubuntu:24.04
LABEL com.example.version="0.0.1-beta"
RUN apt-get update && apt-get install -y --no-install-recommends \
    bzr \
    cvs \
    git \
    mercurial \
    subversion \
    && rm -rf /var/lib/apt/lists/*
RUN set -o pipefail && wget -O - https://some.site | wc -l > /number
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
RUN wget -O - https://some.site | wc -l > /number
ENV PG_MAJOR=9.3
ENV PG_VERSION=9.3.4
RUN curl -SL https://example.com/postgres-$PG_VERSION.tar.xz | tar -xJC /usr/src/postgres && echo
ENV PATH=/usr/local/postgres-$PG_MAJOR/bin:$PATH
VOLUME ["/data"]
STOPSIGNAL SIGTERM
