/**
 * Next.js 16 proxy configuration.
 *
 * In development mode, routes /api/* requests to the axum backend.
 * WebSocket connections bypass this proxy and connect directly to
 * the backend port from the client.
 */
import type { NextRequest} from "next/server";
import { NextResponse } from "next/server";

const BACKEND =
  process.env["CINCH_BACKEND_URL"] ?? "http://127.0.0.1:3001";

export default function proxy(request: NextRequest): NextResponse {
  const { pathname } = request.nextUrl;

  // Route API calls to axum backend.
  if (pathname.startsWith("/api/")) {
    return NextResponse.rewrite(new URL(pathname, BACKEND));
  }

  return NextResponse.next();
}

export const config = {
  matcher: ["/api/:path*"],
};
