/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: true,
  swcMinify: true,
};

module.exports = {
  // e.g. NEXT_PUBLIC_BASE_PATH=/admin to serve the dashboard under a sub-path;
  // src/utils/util.ts prefixes public/ assets with the same value.
  basePath: process.env.NEXT_PUBLIC_BASE_PATH || '',
  webpack(config) {
    config.module.rules.push({
      test: /\.svg$/i,
      issuer: /\.[jt]sx?$/,
      use: ['@svgr/webpack'],
    });

    return config;
  },
  async rewrites() {
    return [
      {
        source: '/:any*',
        destination: '/',
      },
    ];
  },
};
