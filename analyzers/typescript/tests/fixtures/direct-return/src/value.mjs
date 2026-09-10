export const select = ({ items }) => {
  if (!items) return false;
  if (items.length === 0) return "*";
  const projected = items.map((item) => opaque(item));
  if (projected.includes("*")) return "*";
  return projected.map((item) => anotherOpaque(item));
};
