import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import type { PluginOption } from "vite";
// Dev-only fixture API (`npm run dev:mock`). Never registered for `vite build`.
async function mockPlugins(
  command: "build" | "serve",
): Promise<PluginOption[]> {
  if (command !== "serve" || process.env.NIVRA_MOCK !== "1") return [];
  return [(await import("./scripts/mock-api")).mockApi()];
}
export default defineConfig(async ({ command }) => ({
  base: "/",
  plugins: [react(), ...(await mockPlugins(command))],
  build: {
    outDir: "bundle",
    emptyOutDir: true,
    sourcemap: false,
    rollupOptions: {
      output: {
        entryFileNames: "app.js",
        chunkFileNames: "[name].js",
        assetFileNames: "app.[ext]",
      },
    },
  },
}));
