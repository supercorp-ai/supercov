import { Greeting } from '@/components/greeting';
import { getClient } from '@/lib/client';

export const dynamic = 'force-dynamic';

export default function Page() {
  return (
    <main>
      <Greeting name={getClient().ping()} />
    </main>
  );
}
