import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  // React Compiler: automatic memoization — important for the chat UI
  // which re-renders frequently as streaming tokens arrive.
  reactCompiler: true,

  // Static export for production (served by axum as a single binary).
  // Disabled in dev because it conflicts with the proxy middleware.
  ...(process.env.NODE_ENV === "production" ? { output: "export" as const } : {}),
};

export default nextConfig;
