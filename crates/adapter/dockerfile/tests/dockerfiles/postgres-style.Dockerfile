FROM debian:bookworm-slim

RUN set -eux; \
	groupadd -r postgres --gid=999; \
# the comment below is part of the official image style
	useradd -r -g postgres --uid=999 --home-dir=/var/lib/postgresql --shell=/bin/bash postgres; \
	mkdir -p /var/lib/postgresql; \
	chown -R postgres:postgres /var/lib/postgresql

ENV LANG en_US.utf8
ENV PG_MAJOR 16
ENV PATH $PATH:/usr/lib/postgresql/$PG_MAJOR/bin
ENV PGDATA /var/lib/postgresql/data
RUN mkdir -p "$PGDATA" && chown -R postgres:postgres "$PGDATA" && chmod 1777 "$PGDATA"
VOLUME /var/lib/postgresql/data

COPY docker-entrypoint.sh docker-ensure-initdb.sh /usr/local/bin/
ENTRYPOINT ["docker-entrypoint.sh"]
STOPSIGNAL SIGINT
EXPOSE 5432
CMD ["postgres"]
