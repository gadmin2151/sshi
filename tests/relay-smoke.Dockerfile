ARG BASE=debian:bookworm-slim
FROM ${BASE}
COPY sshi.deb /tmp/sshi.deb
RUN rm -f /etc/dpkg/dpkg.cfg.d/docker \
    && apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends /tmp/sshi.deb openssh-server python3 man-db curl \
    && rm -rf /var/lib/apt/lists/* /tmp/sshi.deb
COPY relay_smoke.py /usr/local/lib/sshi-relay-smoke.py
ENTRYPOINT ["python3", "/usr/local/lib/sshi-relay-smoke.py"]
