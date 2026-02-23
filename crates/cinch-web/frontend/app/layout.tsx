import "./globals.css";
import type { Metadata } from "next";

export const metadata: Metadata = {
  title: "Cinch Web",
  description: "Browser-based chat UI for cinch-rs agents",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}): React.ReactNode {
  return (
    <html lang="en">
      <body className="min-h-screen">
        <div className="flex flex-col h-screen">{children}</div>
      </body>
    </html>
  );
}
