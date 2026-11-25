FROM oven/bun:1.4.5-slim
ARG VERSION=dev
LABEL org.opencontainers.image.title="pdfsign-bot" org.opencontainers.image.version="${VERSION}"
ENV PDFSIGN_BOT_VERSION=${VERSION}
WORKDIR /app
COPY package.json bun.lock ./
RUN bun install --frozen-lockfile --production
COPY src ./src
USER bun
CMD ["bun", "run", "src/index.ts"]
