FROM oven/bun:1.4.5 AS build
WORKDIR /app
COPY package.json bun.lock ./
RUN bun install --frozen-lockfile
COPY . .
RUN bun run build

FROM oven/bun:1.4.5-slim
ARG VERSION=dev
LABEL org.opencontainers.image.title="pdfsign-calendar" org.opencontainers.image.version="${VERSION}"
WORKDIR /app
COPY package.json bun.lock ./
RUN bun install --frozen-lockfile --production \
    && mkdir /data && chown bun:bun /data
COPY --from=build /app/dist ./dist
COPY server ./server
COPY src/lib/events.ts ./src/lib/events.ts
ENV NODE_ENV=production CALENDAR_HOST=0.0.0.0 PORT=3090 CALENDAR_DATA_DIR=/data
USER bun
EXPOSE 3090
HEALTHCHECK --interval=30s --timeout=5s CMD ["bun", "-e", "if (!(await fetch('http://127.0.0.1:3090/health')).ok) process.exit(1)"]
CMD ["bun", "run", "start"]
