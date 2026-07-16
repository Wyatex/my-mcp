FROM rust:alpine AS chef
RUN apk add --no-cache musl-dev
RUN cargo install cargo-chef
WORKDIR /app

# 规划阶段：生成依赖配方
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# 构建阶段：先编译依赖，再编译源码
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# 这里等同于你那个假 main.rs 的作用，只编译依赖！
RUN cargo chef cook --release --recipe-path recipe.json 

# 拷贝真实代码并编译最终产物
COPY . .
RUN cargo build --release

# 运行阶段
FROM gcr.io/distroless/static-debian12:latest
COPY --from=builder /app/target/release/kimi-mcp-rust /app/kimi-mcp-rust
EXPOSE 3000
CMD ["/app/kimi-mcp-rust"]