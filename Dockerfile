# python letters.py && docker build -t build-harlem-shake . && docker run -d --name build-harlem-shake build-harlem-shake && docker cp $(docker ps -aqf "name=^build-harlem-shake$"):/harlem_shake/target/release/harlem_shake harlem_shake && docker cp $(docker ps -aqf "name=^build-harlem-shake$"):/harlem_shake/target/debug/harlem_shake harlem_shake_debug && docker rm build-harlem-shake

FROM debian:bookworm

RUN DEBIAN_FRONTEND=noninteractive apt-get update && \
    apt-get install -y \
        curl build-essential pkg-config libudev-dev \
    && rm -rf /var/lib/apt/lists/

RUN curl https://sh.rustup.rs -sSf | bash -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

RUN mkdir -p /harlem_shake
WORKDIR /harlem_shake

COPY letters/ /harlem_shake/letters/
COPY Cargo.toml Cargo.lock letters.py /harlem_shake/
COPY src/ /harlem_shake/src/

RUN cargo build
RUN cargo build --release
