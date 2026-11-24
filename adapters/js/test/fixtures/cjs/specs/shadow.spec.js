const { findItems } = require('../utils/helpers');

function run(findItems) {
  findItems(1);
}

let later = require('../utils/helpers');
later = null;
later.findItems(2);
run(() => {});
