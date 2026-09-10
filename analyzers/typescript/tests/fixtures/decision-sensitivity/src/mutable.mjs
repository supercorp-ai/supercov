export function mutableChoice(flag) {
  if (flag) {
    return 1;
  } else {
    return 2;
  }
}

export function replaceChoice() {
  mutableChoice = () => 0;
}
