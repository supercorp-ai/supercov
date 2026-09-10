export function countOnly() {
  console.log('count payload', 1);
}
export function countLength() {
  console.log('length payload');
}
export function argumentsOnly() {
  console.log('argument payload', 1);
}
export function selectedCall() {
  console.log('selected first');
  console.log('selected second');
}
export function slicedCall() {
  console.log('slice first');
  console.log('slice second');
}
export function mappedCalls() {
  console.log('mapped first', 1);
  console.log('mapped second', 2);
}
export function conditionalMocks() {
  console.log('conditional left');
  console.error('conditional right');
}
export function resetHistory() {
  console.log('reset payload');
}
export function lateMock() {
  console.log('late payload');
}
export function failedWitness() {
  console.log('failed payload');
}
export function inactiveWitness() {
  console.log('inactive payload');
}
export function fakeTracker() {
  console.log('fake tracker payload');
}
export function shadowedReceiver() {
  console.log('shadowed receiver payload');
}
