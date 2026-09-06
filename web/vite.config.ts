import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid()],
  server: {
    host: "127.0.0.1",
    port: 5173,
    proxy: {
      "/v1": "http://127.0.0.1:8643",
      "/.well-known/nexus": "http://127.0.0.1:8643",
    },
  },
  build: {
    target: "es2022",
    cssMinify: "lightningcss",
  },
});
