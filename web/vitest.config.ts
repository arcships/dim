import { defineConfig } from "vitest/config"
import vue from "@vitejs/plugin-vue"
import app from "./vite.config"

export default defineConfig({
  plugins: [vue()],
  resolve: app.resolve,
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts"],
  },
})
