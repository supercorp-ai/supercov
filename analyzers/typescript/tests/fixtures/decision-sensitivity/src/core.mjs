export function identical(sameFlag) {
  if (sameFlag) {
    return 7;
  } else {
    return 7;
  }
}

export function distinct(distinctFlag) {
  if (distinctFlag) {
    return 1;
  } else {
    return 2;
  }
}

export function masked(maskedFlag) {
  if (maskedFlag) {
    return 1;
  } else {
    return 2;
  }
}

export function transformed(transformedFlag) {
  if (transformedFlag) {
    return -1;
  } else {
    return 1;
  }
}

export function effectful(effectfulFlag, events) {
  if (effectfulFlag) {
    events.push("left");
    return 7;
  } else {
    events.push("right");
    return 7;
  }
}
