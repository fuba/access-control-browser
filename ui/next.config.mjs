// Static export for daemon embedding. The daemon serves the contents of
// ui/out/ at the root via rust-embed.
/** @type {import('next').NextConfig} */
const nextConfig = {
  output: "export",
  trailingSlash: true,
  images: { unoptimized: true },
};
export default nextConfig;
