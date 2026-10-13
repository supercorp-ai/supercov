export function total(price, quantity) {
  return price * quantity;
}

// No test asks for the half-price code, so the workflow that runs the action on this project has a
// changed line to report.
export function discounted(price, code) {
  if (code === 'half') {
    return price / 2;
  }
  return price;
}
