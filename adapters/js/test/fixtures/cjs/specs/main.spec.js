const path = require('path');
const { findItems, waitFor: wait } = require('../utils/helpers');
const helpers = require('../utils/helpers');
const recorder = require('../utils/recorder');
const other = require('../utils/other-recorder');
const APage = require('../pages/a.page');
const BPage = require('../pages/b.page');
const lib = require('../lib');
const barrel = require('../lib/barrel');
const data = require('../data.json');

function reset() {}

it('uses everything', async () => {
  await findItems(1);
  wait(2);
  helpers.waitFor(3);
  require('../utils/helpers').findItems(4);
  recorder.reset();
  other.reset();
  reset();
  APage.submit();
  BPage.submit();
  lib.hello();
  barrel.record();
  barrel.findItems(5);
  path.join('a', 'b');
  data.thing();
  unknownGlobal();
});
