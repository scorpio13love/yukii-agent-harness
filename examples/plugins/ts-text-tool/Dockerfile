# Build stage: TypeScript -> dist/*.js
FROM node:22-slim AS build
WORKDIR /app
COPY package.json tsconfig.json ./
COPY src ./src
RUN npm install && npm run build

# Runtime stage: no npm install, no shell entrypoint, no host files.
FROM node:22-slim
WORKDIR /app
COPY --from=build /app/dist ./dist
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/node", "/app/dist/index.js"]
