import { label } from '@/lib/client';

export async function POST(request: Request) {
  const body = (await request.json()) as { count: number };
  return Response.json({ ok: true, label: label(body.count, true) });
}
