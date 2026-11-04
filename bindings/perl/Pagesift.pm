package Pagesift;
# libpagesift 的 Perl 绑定，用 FFI::Platypus（CPAN）调用 C ABI。
# 函数返回 Document JSON 字符串，不解析。
# 库位置：环境变量 PAGESIFT_LIB（库文件完整路径）；没设时用仓库里的 target/f/quick/libpagesift.so。
use strict;
use warnings;
use FFI::Platypus 2.00;
use FFI::Platypus::Buffer qw(scalar_to_buffer);
use File::Basename qw(dirname);
use File::Spec;

my $lib = $ENV{PAGESIFT_LIB}
  // File::Spec->catfile(dirname(__FILE__), '..', '..', 'target', 'f', 'quick', 'libpagesift.so');

my $ffi = FFI::Platypus->new(api => 2, lib => $lib);
$ffi->attach(['pagesift_version'     => '_version'],     [] => 'string');
$ffi->attach(['pagesift_string_free' => '_string_free'], ['opaque'] => 'void');
$ffi->attach(['pagesift_engine_new'  => '_engine_new'],  ['string', 'opaque*'] => 'opaque');
$ffi->attach(['pagesift_engine_free' => '_engine_free'], ['opaque'] => 'void');
$ffi->attach(['pagesift_convert'     => '_convert'],     ['string', 'string'] => 'opaque');
$ffi->attach(['pagesift_scrape'      => '_scrape'],      ['opaque', 'string', 'string'] => 'opaque');
$ffi->attach(['pagesift_parse'       => '_parse'],
  ['opaque', 'opaque', 'size_t', 'string', 'string'] => 'opaque');

# 把库返回的 char* 复制成 Perl 字符串（UTF-8 解码后的字符），并释放库里的那一份。
sub _take {
  my ($p) = @_;
  return undef unless $p;
  my $s = $ffi->cast('opaque' => 'string', $p);
  _string_free($p);
  utf8::decode($s);
  return $s;
}

sub version { _version() }

sub convert {
  my ($html, $options_json) = @_;
  utf8::encode(my $h = $html);
  return _take(_convert($h, $options_json));
}

# 建引擎；$config_json 为 undef 用默认配置。配置不合法时 die。
sub engine {
  my ($config_json) = @_;
  my $err;
  my $e = _engine_new($config_json, \$err);
  unless ($e) {
    my $msg = _take($err) // 'engine_new failed';
    die "$msg\n";
  }
  return bless { h => $e }, 'Pagesift::Engine';
}

package Pagesift::Engine;

sub _live {
  my ($self) = @_;
  die "engine is closed\n" unless $self->{h};
  return $self->{h};
}

# $data 是字节串。
sub parse {
  my ($self, $data, $filename, $options_json) = @_;
  my $h = $self->_live;
  my ($ptr, $len) = length($data) ? FFI::Platypus::Buffer::scalar_to_buffer($data) : (undef, 0);
  return Pagesift::_take(Pagesift::_parse($h, $ptr, $len, $filename, $options_json));
}

sub scrape {
  my ($self, $url, $options_json) = @_;
  return Pagesift::_take(Pagesift::_scrape($self->_live, $url, $options_json));
}

sub close {
  my ($self) = @_;
  if (my $h = delete $self->{h}) { Pagesift::_engine_free($h) }
}

sub DESTROY { $_[0]->close }

1;
