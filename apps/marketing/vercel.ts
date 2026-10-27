import type { VercelConfig } from "@vercel/config/v1";

export const config: VercelConfig = {
  installCommand: "npm install -g vite-plus && vp install --filter '@coddy/marketing'",
  buildCommand: "vp run --filter @coddy/marketing build",
  outputDirectory: "dist",
};
