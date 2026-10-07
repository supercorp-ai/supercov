export function Greeting({ name }: { name: string }) {
  return <h1>{name ? `Hello ${name}` : 'Hello'}</h1>;
}
