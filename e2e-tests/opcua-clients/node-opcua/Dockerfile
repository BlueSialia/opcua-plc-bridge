FROM node:22-alpine

WORKDIR /app
COPY package.json ./
RUN npm install

COPY test-runner.js ./

ENTRYPOINT ["node", "test-runner.js"]
