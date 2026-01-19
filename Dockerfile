# Use the official Rust image as a build environment
FROM rust:1.83 as builder

WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Copy source code
COPY src ./src
COPY scripts ./scripts

# Build the application
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the binary from builder
COPY --from=builder /app/target/release/jamcraft /app/jamcraft
COPY --from=builder /app/target/release/spotify_auth /app/spotify_auth

# Expose port
EXPOSE 3000

# Run the application
CMD ["./jamcraft"]
