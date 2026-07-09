import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In dev, proxy API calls to the zoomy core service.
export default defineConfig({
  plugins: [react()],
  build: {
    rollupOptions: {
      output: {
        // React in a stable vendor chunk so it stays cached across app deploys
        // (only the app chunk's hash changes). Pages are code-split via
        // React.lazy in App.tsx.
        manualChunks: {
          react: ["react", "react-dom"],
        },
      },
    },
  },
  server: {
    proxy: {
      "/api": { target: "http://localhost:8080", ws: true },
    },
  },
});
