export const runtime = 'edge';

function grade(value: number) {
  if (value > 10) {
    return 'high';
  }
  return 'low';
}

export async function GET(request: Request) {
  const value = Number(new URL(request.url).searchParams.get('value') ?? '0');
  return Response.json({ grade: grade(value) });
}
