xhr = new XMLHttpRequest();
				xhr.open( "GET", "https://deepseek.de/wp-admin/admin-ajax.php?action=pll_xdata_get&redirect=https%3A%2F%2Fdeepseek.es%2F&nonce=0a6dca9b00", true );
				xhr.withCredentials = true;
				xhr.onreadystatechange = function () {
					if ( 4 == this.readyState && 200 == this.status && this.responseText && -1 != this.responseText ) {
						window.location.replace( "https://deepseek.es/wp-admin/admin-ajax.php?action=pll_xdata_set" + "&key=" + this.responseText );
					}
				}
				xhr.send();