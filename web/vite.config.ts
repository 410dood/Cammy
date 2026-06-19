import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In dev, proxy API calls to the zoomy core service.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api": { target: "http://localhost:8080", ws: true },
    },
  },
});
