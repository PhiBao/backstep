/** @type {import('next').NextConfig} */
const backend = process.env.BACKSTEP_BACKEND || "http://127.0.0.1:8787";
const nextConfig = {
  async rewrites() {
    return [
      { source: "/api/:path*", destination: `${backend}/api/:path*` },
      { source: "/health", destination: `${backend}/health` },
    ];
  },
};
export default nextConfig;
