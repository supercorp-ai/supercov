import { NextResponse, type NextRequest } from 'next/server';

export function proxy(request: NextRequest) {
  const response = NextResponse.next();
  response.headers.set('x-path', request.nextUrl.pathname);
  return response;
}

export const config = { matcher: ['/api/:path*'] };
