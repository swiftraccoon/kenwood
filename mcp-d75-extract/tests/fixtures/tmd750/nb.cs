public class nb
{
	private int c;

	public int OffsetProgrammableMemoryAddress
	{
		set
		{
			c = value;
		}
	}

	public bool Dhcp
	{
		get { return false; }
	}

	public string IpAddress
	{
		get { return string.Empty; }
	}

	public bool WirelessLan
	{
		get { return false; }
	}

	public void a6(n7 A_0)
	{
		A_0.a(Convert.ToByte(Dhcp), 332855 + c);
		A_0.a(a(IpAddress), 332856 + c);
		A_0.a(Convert.ToByte(WirelessLan), 332876 + c);
	}

	public void a7(n7 A_0)
	{
		Dhcp = A_0.a(332855 + c) != 0;
	}

	private byte[] a(string A_0)
	{
		return new byte[4];
	}
}
