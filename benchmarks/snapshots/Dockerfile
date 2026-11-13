FROM rust:1.89.0-bookworm@sha256:c9ac3fa8945b61dede1e4500d25028aa8fd8a8fe46365fcf9c0422f8d999b9b0
# Acquisition phase. Versions and archives are immutable; compilation below is offline.
RUN rm /etc/apt/sources.list.d/debian.sources \
 && printf '%s\n' 'deb [check-valid-until=no] https://snapshot.debian.org/archive/debian/20250901T000000Z bookworm main' > /etc/apt/sources.list \
 && apt-get -o Acquire::Check-Valid-Until=false update \
 && apt-get install -y --no-install-recommends time ccache=4.8+really4.7.5-1 \
 && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -fsSL https://www.kernel.org/pub/software/scm/git/git-2.40.0.tar.xz -o /tmp/git.tar.xz \
 && echo 'b17a598fbf58729ef13b577465eb93b2d484df1201518b708b5044ff623bf46d  /tmp/git.tar.xz' | sha256sum -c - \
 && tar -xf /tmp/git.tar.xz -C /tmp \
 && make -C /tmp/git-2.40.0 -j4 prefix=/opt/git NO_TCLTK=YesPlease NO_GETTEXT=YesPlease NO_CURL=YesPlease NO_OPENSSL=YesPlease NO_EXPAT=YesPlease install \
 && rm -rf /tmp/git-2.40.0 /tmp/git.tar.xz
ENV PATH="/opt/git/bin:${PATH}" GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_OPTIONAL_LOCKS=0 LC_ALL=C TZ=UTC CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 SOURCE_DATE_EPOCH=1735689600
RUN rustup component add clippy rustfmt
WORKDIR /workspace
COPY . .
RUN mv baseline /baseline
# Directory overrides outrank contributor rust-toolchain.toml files and survive
# the benchmark runner's environment sanitization, including isolated HOME.
RUN rustup override set 1.89.0 && cd /baseline && rustup override set 1.89.0
COPY --chmod=0644 benchmark-source.json /opt/benchmark-source.json
RUN cargo vendor --locked --versioned-dirs --sync /baseline/Cargo.toml /vendor > /usr/local/cargo/config.toml \
 && find /vendor -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum > /opt/benchmark-vendor.sha256
# BuildKit enforces network isolation, including build scripts. Frozen also prevents lock drift.
RUN --network=none cargo build -p groove-bench --release --frozen \
 && cargo build -p groove-cli --release --frozen \
 && target/release/groove-bench verify-environment \
 && cd /baseline && cargo build -p groove-cli --release --frozen
RUN dpkg-query -W > /opt/benchmark-packages.txt
ENTRYPOINT ["/workspace/target/release/groove-bench"]
