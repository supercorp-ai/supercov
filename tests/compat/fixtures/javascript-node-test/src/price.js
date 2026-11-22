export function price(amount, member, coupon) {
  let total = amount;
  if (member && amount > 100) {
    total = total * 0.9;
  }
  if (coupon) {
    total = total - 5;
  }
  return Math.max(total, 0);
}

export function label(total) {
  return total > 50 ? "large" : "small";
}

export function unused(value) {
  for (const item of value) {
    if (item < 0) return false;
  }
  return true;
}
