import { getClient } from '@/lib/client';

export function GET() {
  return Response.json({ reply: getClient().ping() });
}
