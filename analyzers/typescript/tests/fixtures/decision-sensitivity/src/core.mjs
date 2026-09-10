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

export function zero(zeroFlag) {
  if (zeroFlag) {
    return -0;
  } else {
    return 0;
  }
}

export function typed(typedFlag) {
  if (typedFlag) {
    return "1";
  } else {
    return 1;
  }
}

export function lone(loneFlag) {
  if (loneFlag) {
    return "\ud800";
  } else {
    return "\ud800";
  }
}
