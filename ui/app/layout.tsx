import "./globals.css";

export const metadata = {
  title: "access-control-browser",
  description: "Access-controlled browser for LLM coding agents",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
