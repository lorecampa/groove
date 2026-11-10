FROM rust:1.89.0-bookworm@sha256:c9ac3fa8945b61dede1e4500d25028aa8fd8a8fe46365fcf9c0422f8d999b9b0
# Immutable Debian repository date; the image digest pins the inherited packages.
RUN rm /etc/apt/sources.list.d/debian.sources \
 && printf '%s\n' 'deb [check-valid-until=no] https://snapshot.debian.org/archive/debian/20250901T000000Z bookworm main' > /etc/apt/sources.list \
 && apt-get -o Acquire::Check-Valid-Until=false update \
 && apt-get install -y --no-install-recommends time \
 && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -fsSL https://www.kernel.org/pub/software/scm/git/git-2.40.0.tar.xz -o /tmp/git.tar.xz \
 && echo 'b17a598fbf58729ef13b577465eb93b2d484df1201518b708b5044ff623bf46d  /tmp/git.tar.xz' | sha256sum -c - \
 && tar -xf /tmp/git.tar.xz -C /tmp \
 && make -C /tmp/git-2.40.0 -j4 prefix=/opt/git NO_TCLTK=YesPlease NO_GETTEXT=YesPlease NO_CURL=YesPlease NO_OPENSSL=YesPlease NO_EXPAT=YesPlease install \
 && rm -rf /tmp/git-2.40.0 /tmp/git.tar.xz
ENV PATH="/opt/git/bin:${PATH}" GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_OPTIONAL_LOCKS=0 LC_ALL=C TZ=UTC CARGO_BUILD_JOBS=4
WORKDIR /workspace
COPY . .
COPY --chmod=0644 benchmark-source.json /opt/benchmark-source.json
RUN cargo build -p groove-bench -p groove-cli --release --locked \
 && target/release/groove-bench verify-environment \
 && cargo test -p groove-core --test snapshot_scale --release --locked --no-run
# Runtime remains offline. Build provenance and exact installed packages are retained.
RUN dpkg-query -W > /opt/benchmark-packages.txt
ENTRYPOINT ["/workspace/target/release/groove-bench"]
