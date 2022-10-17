FROM rust:1.64.0 as build

RUN USER=root cargo new --bin hello
WORKDIR /hello

COPY ./Cargo.lock ./Cargo.lock
COPY ./Cargo.toml ./Cargo.toml
RUN cargo build --release

RUN rm src/*.rs
COPY ./src ./src

RUN rm ./target/release/deps/hello*
RUN cargo update \
    && cargo build --release

FROM rust:1.64.0-slim
COPY --from=build /hello/target/release/hello .
COPY ./web_config.json .

EXPOSE 80
CMD ["./hello"]

