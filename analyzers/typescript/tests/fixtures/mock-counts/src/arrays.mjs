export function mappedCalls() {
  ['a', 'b'].map((value, index, array) => {
    console.log(value, index, array.length);
    return value;
  });
}
export function mappedRoutes() {
  ['a', 'b'].map((value, index) => {
    if (index === 0) console.log(value);
    else console.error(value);
    return value;
  });
}
export function mappedOutput() {
  const output = ['a', 'b'].map((value) => ({ value, tag: 'mapped payload' }));
  console.log(output);
}
const values = () => { console.log('receiver evaluated'); return ['a', 'b']; };
const mapper = () => {
  console.log('callback evaluated');
  return (value) => { console.log('mapped', value); return value; };
};
export function mappedEvaluation() { values().map(mapper()); }
export function ownMap() {
  const receiver = { map: () => console.error('own map') };
  receiver.map((value) => console.log(value));
}
export function sliceIndex() { console.log('slice argument evaluated'); return 0; }
export function sparseMap() { [, 'a'].map((value) => console.log(value)); }
export function mutatingMap() {
  const values = ['a'];
  values.map((value) => { values.push('b'); console.log(value); });
}
export function mapWithThis() {
  ['a'].map(function () { console.log(this.label); }, { label: 'this argument' });
}
export function slicedArray() { console.log(...['before', 'selected', 'after'].slice(1, 2)); }
